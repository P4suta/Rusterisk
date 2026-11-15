use rusterisk_client::{
    api::{ChannelId, EventKind},
    CallMilestone, Client, Error, Originate, OwnedCall, Session,
};
use std::{future::Future, time::Duration};

#[cfg(test)]
mod tests;

struct Bridge {
    id: ChannelId,
    name: String,
}

struct Conversation {
    incoming: Option<OwnedCall>,
    peer: Option<OwnedCall>,
    bridge: Option<Bridge>,
}

fn absent(result: Result<(), Error>) -> Result<(), Error> {
    match result {
        Err(Error::Api { status: 404, .. }) => Ok(()),
        result => result,
    }
}

fn combine(results: impl IntoIterator<Item = Result<(), Error>>) -> Result<(), Error> {
    let failures: Vec<String> = results
        .into_iter()
        .filter_map(Result::err)
        .map(|error| error.to_string())
        .collect();
    if failures.is_empty() {
        Ok(())
    } else {
        Err(Error::Cleanup(failures.join("; ")))
    }
}

impl Conversation {
    fn new(incoming: OwnedCall) -> Self {
        Self {
            incoming: Some(incoming),
            peer: None,
            bridge: None,
        }
    }

    fn owns(&self, id: &ChannelId) -> bool {
        self.incoming.as_ref().is_some_and(|call| call.id() == id)
            || self.peer.as_ref().is_some_and(|peer| peer.id() == id)
    }

    async fn begin(
        &mut self,
        client: &Client,
        session: &Session,
        endpoint: &str,
    ) -> Result<bool, Error> {
        let incoming = self.incoming.as_ref().ok_or(Error::Closed)?.id().clone();
        client.answer(&incoming).await?;
        match self
            .incoming
            .as_mut()
            .ok_or(Error::Closed)?
            .wait(CallMilestone::Answered, Duration::from_secs(30))
            .await
        {
            Ok(_) => {}
            Err(Error::StasisExited | Error::CallEnded(_)) => return Ok(false),
            Err(error) => return Err(error),
        }
        let identity = uuid::Uuid::new_v4().to_string();
        self.bridge = Some(Bridge {
            id: ChannelId::try_from(identity.clone())?,
            name: format!("browser-{identity}"),
        });
        let bridge = self.bridge.as_ref().ok_or(Error::Closed)?;
        let created = client.create_bridge(&bridge.id, &bridge.name).await?;
        if created.id != bridge.id || created.name != bridge.name {
            return Err(Error::Invalid(
                "bridge creation returned a different identity",
            ));
        }
        client
            .add_channels(&bridge.id, std::slice::from_ref(&incoming))
            .await?;
        self.peer = Some(
            session
                .originate(Originate::new(endpoint.to_owned())?)
                .await?,
        );
        let peer = self.peer.as_mut().ok_or(Error::Closed)?;
        let entered = self.incoming.as_mut().ok_or(Error::Closed)?;
        let answered = tokio::select! {
            result = peer.wait(CallMilestone::Stasis, Duration::from_secs(30)) => { result?; true }
            result = entered.wait_completion(Duration::from_secs(30)) => { result?; false }
        };
        if !answered {
            return Ok(false);
        }
        client
            .add_channels(&bridge.id, std::slice::from_ref(peer.id()))
            .await?;
        Ok(true)
    }

    async fn close(mut self, client: &Client) -> Result<(), Error> {
        let mut results = Vec::with_capacity(3);
        if let Some(peer) = self.peer.take() {
            results.push(peer.close().await);
        }
        if let Some(incoming) = self.incoming.take() {
            results.push(incoming.close().await);
        }
        if let Some(bridge) = self.bridge.take() {
            let result = match client.bridge(&bridge.id).await {
                Ok(observed) if observed.id == bridge.id && observed.name == bridge.name => {
                    absent(client.destroy_bridge(&bridge.id).await)
                }
                Ok(_) => Err(Error::Invalid("bridge identity changed before cleanup")),
                Err(Error::Api { status: 404, .. }) => Ok(()),
                Err(error) => Err(error),
            };
            results.push(result);
        }
        combine(results)
    }
}

async fn events(
    client: &Client,
    session: &mut Session,
    endpoint: &str,
    active: &mut Option<Conversation>,
) -> Result<(), Error> {
    loop {
        let event = session.next_event().await?;
        if let EventKind::StasisStart { channel, .. } = event.kind {
            if active.as_ref().is_some_and(|call| call.owns(&channel.id)) {
                continue;
            }
            let incoming = match session.adopt(&channel.id).await {
                Ok(incoming) => incoming,
                Err(Error::Invalid(_)) => continue,
                Err(error) => return Err(error),
            };
            if active.is_some() {
                incoming.close().await?;
                continue;
            }
            *active = Some(Conversation::new(incoming));
            let started = active
                .as_mut()
                .ok_or(Error::Closed)?
                .begin(client, session, endpoint)
                .await?;
            if !started {
                active.take().ok_or(Error::Closed)?.close(client).await?;
            }
            continue;
        }
        let ended = if let EventKind::StasisEnd { channel } = &event.kind {
            Some(&channel.id)
        } else if let EventKind::ChannelDestroyed { channel, .. } = &event.kind {
            Some(&channel.id)
        } else {
            None
        };
        if let Some(ended) = ended {
            if active.as_ref().is_some_and(|call| call.owns(ended)) {
                let mut call = active.take().ok_or(Error::Closed)?;
                if call
                    .incoming
                    .as_ref()
                    .is_some_and(|call| call.id() == ended)
                {
                    call.incoming = None;
                }
                call.close(client).await?;
            }
        }
    }
}

pub(super) async fn run(
    client: Client,
    application: ChannelId,
    endpoint: String,
    stop: impl Future<Output = Result<(), Error>>,
) -> Result<(), Error> {
    Originate::new(endpoint.clone())?;
    let mut session = client.subscribe(&application).await?;
    let mut active = None;
    println!("Browser bridge controller ready.");
    let operation = tokio::select! {
        result = events(&client, &mut session, &endpoint, &mut active) => result,
        result = stop => result,
    };
    let cleanup = match active.take() {
        Some(call) => call.close(&client).await,
        None => Ok(()),
    };
    combine([operation, cleanup, session.shutdown().await])
}
