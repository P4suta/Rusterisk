use crate::{
    api,
    owned_call::{self, Calls},
    Client, Error, Originate, OwnedCall,
};
use futures_util::{SinkExt, StreamExt};
use std::time::Duration;
use tokio::sync::{broadcast, watch};
use tokio_tungstenite::tungstenite::{
    client::IntoClientRequest, protocol::WebSocketConfig, Message,
};

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

pub struct Session {
    requests: owned_call::Requests,
    events: broadcast::Receiver<api::Event>,
    stop: watch::Sender<bool>,
    task: Option<tokio::task::JoinHandle<Result<(), Error>>>,
}

impl Session {
    /// Adopts a channel observed in the session's Stasis application.
    /// The handle preserves observed answer and media milestones.
    /// Incoming adoption has no HTTP acceptance milestone.
    pub async fn adopt(&self, id: &api::ChannelId) -> Result<OwnedCall, Error> {
        owned_call::adopt(&self.requests, id.clone()).await
    }
    pub async fn originate(&self, options: Originate) -> Result<OwnedCall, Error> {
        owned_call::originate(&self.requests, options).await
    }
    pub async fn external_websocket(
        &self,
        options: api::WebSocketMediaOptions,
        data: &str,
    ) -> Result<OwnedCall, Error> {
        owned_call::external_websocket(&self.requests, options, data).await
    }
    pub async fn external_rtp(
        &self,
        target: api::ExternalRtpTarget,
        data: &str,
    ) -> Result<OwnedCall, Error> {
        owned_call::external_rtp(&self.requests, target, data).await
    }
    pub async fn external_audio_socket(
        &self,
        target: api::AudioSocketTarget,
    ) -> Result<OwnedCall, Error> {
        owned_call::external_audio_socket(&self.requests, target).await
    }
    pub async fn next_event(&mut self) -> Result<api::Event, Error> {
        match self.events.recv().await {
            Ok(event) => Ok(event),
            Err(broadcast::error::RecvError::Lagged(count)) => Err(Error::EventLagged(count)),
            Err(broadcast::error::RecvError::Closed) => {
                let result = match self.task.as_mut() {
                    Some(task) => task.await.map_err(|_| Error::Closed)?,
                    None => return Err(Error::Closed),
                };
                self.task = None;
                result?;
                Err(Error::Closed)
            }
        }
    }

    pub async fn shutdown(mut self) -> Result<(), Error> {
        self.stop.send_replace(true);
        let task = self.task.take().ok_or(Error::Closed)?;
        task.await.map_err(|_| Error::Closed)?
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.stop.send_replace(true);
    }
}

pub(crate) async fn connect(
    client: &Client,
    application: &api::ChannelId,
) -> Result<Session, Error> {
    let mut url = client.base.clone();
    let scheme = match url.scheme() {
        "https" => "wss",
        "http" => "ws",
        _ => return Err(Error::Invalid("invalid event transport")),
    };
    url.set_scheme(scheme)
        .map_err(|()| Error::Invalid("invalid event transport"))?;
    url.set_path("/ari/events");
    url.query_pairs_mut()
        .append_pair("app", application.as_str());
    let mut request = url.as_str().into_client_request()?;
    request
        .headers_mut()
        .insert("Authorization", client.authorization.clone());
    request.headers_mut().insert(
        "Sec-WebSocket-Protocol",
        reqwest::header::HeaderValue::from_static("ari"),
    );
    let config = WebSocketConfig::default()
        .max_message_size(Some(65536))
        .max_frame_size(Some(65536))
        .write_buffer_size(0)
        .max_write_buffer_size(131072);
    let (socket, response) = tokio::time::timeout(
        Duration::from_secs(5),
        tokio_tungstenite::connect_async_with_config(request, Some(config), true),
    )
    .await
    .map_err(|_| Error::Timeout)??;
    if response
        .headers()
        .get("Sec-WebSocket-Protocol")
        .is_some_and(|value| value != "ari")
    {
        return Err(Error::Invalid("invalid event subprotocol"));
    }
    let (events, received) = broadcast::channel(64);
    let (stop, stopped) = watch::channel(false);
    let (requests, calls) = Calls::new(client.clone(), application.clone());
    let task = tokio::spawn(async move {
        let result = run(socket, events, stopped, calls).await;
        if let Err(error) = &result {
            tracing::warn!(%error, "client session stopped with an error");
        }
        result
    });
    Ok(Session {
        requests,
        events: received,
        stop,
        task: Some(task),
    })
}

async fn run(
    mut socket: Socket,
    events: broadcast::Sender<api::Event>,
    mut stop: watch::Receiver<bool>,
    mut calls: Calls,
) -> Result<(), Error> {
    let period = Duration::from_secs(20);
    let mut heartbeat = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut last_received = tokio::time::Instant::now();
    let result = async {
        loop {
            if *stop.borrow() { break; }
            tokio::select! {
                command = calls.commands.recv() => match command { Some(command) => calls.submit(command), None => break },
                worker = calls.originates.join_next(), if !calls.originates.is_empty() => {
                    if let Some(worker) = worker { let (id, result) = worker.map_err(|_| Error::Closed)?; calls.originated(id, result); }
                }
                worker = calls.cleanups.join_next(), if !calls.cleanups.is_empty() => {
                    if let Some(worker) = worker { let (id, result) = worker.map_err(|_| Error::Closed)?; calls.cleaned(id, result); }
                }
                worker = calls.probes.join_next(), if !calls.probes.is_empty() => {
                    if let Some(worker) = worker { let (id, result) = worker.map_err(|_| Error::Closed)?; calls.probed(id, result); }
                }
                Some(()) = calls.closed.recv() => calls.settle(),
                changed = stop.changed() => if changed.is_err() || *stop.borrow() { break; },
                message = socket.next() => {
                    let message = message.ok_or(Error::Closed)??;
                    last_received = tokio::time::Instant::now();
                    match message {
                        Message::Text(text) => {
                            let event = serde_json::from_str(&text)?;
                            calls.event(&event)?;
                            events.send(event).map_err(|_| Error::Closed)?;
                        }
                        Message::Ping(bytes) => send(&mut socket, Message::Pong(bytes)).await?,
                        Message::Pong(_) => {},
                        Message::Close(_) => return Err(Error::Closed),
                        Message::Binary(_) | Message::Frame(_) => return Err(Error::Invalid("expected a JSON event")),
                    }
                }
                _instant = heartbeat.tick() => {
                    if last_received.elapsed() >= Duration::from_secs(60) { return Err(Error::Timeout); }
                    send(&mut socket, Message::Ping(Vec::new().into())).await?;
                }
            }
        }
        Ok(())
    }.await;
    let cleanup = calls.close().await;
    let close = send(&mut socket, Message::Close(None)).await;
    result.and(cleanup).and(close)
}

async fn send(socket: &mut Socket, message: Message) -> Result<(), Error> {
    tokio::time::timeout(Duration::from_secs(2), socket.send(message))
        .await
        .map_err(|_| Error::Timeout)??;
    Ok(())
}
