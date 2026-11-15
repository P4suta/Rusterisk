use super::{
    api, AdoptRequest, CallSnapshot, Calls, Cleanup, Command, Entry, Error, Lifetime, OwnedCall,
    OwnedCallProgress, Probe, Work,
};
use tokio::sync::{mpsc, oneshot, watch};

pub(crate) async fn adopt(
    commands: &mpsc::Sender<Command>,
    id: api::ChannelId,
) -> Result<OwnedCall, Error> {
    let (progress, received) = watch::channel(CallSnapshot {
        progress: OwnedCallProgress::new(),
        channel: None,
        failure: None,
        cleanup: Cleanup::Pending,
        media_probe_failure: None,
    });
    let (lifetime, dropped) = oneshot::channel();
    let (reply, accepted) = oneshot::channel();
    commands
        .try_send(Command::Adopt(AdoptRequest {
            id: id.clone(),
            progress,
            lifetime: dropped,
            reply,
        }))
        .map_err(|error| match error {
            mpsc::error::TrySendError::Full(_) => Error::Capacity,
            mpsc::error::TrySendError::Closed(_) => Error::Closed,
        })?;
    accepted.await.map_err(|_| Error::Closed)??;
    Ok(OwnedCall {
        id,
        progress: received,
        lifetime: Lifetime::Owned(lifetime),
    })
}

impl Calls {
    pub(super) fn adopt(&mut self, mut request: AdoptRequest) {
        if request.reply.is_closed()
            || !matches!(
                request.lifetime.try_recv(),
                Err(oneshot::error::TryRecvError::Empty)
            )
        {
            return;
        }
        let snapshot = if self.used.contains(&request.id) {
            Err(Error::Invalid("channel already claimed by the session"))
        } else if self.entries.len() >= 64 || self.used.len() >= 4096 {
            Err(Error::Capacity)
        } else {
            self.incoming
                .get(&request.id)
                .cloned()
                .ok_or(Error::Invalid("channel has no live Stasis observation"))
        };
        let snapshot = match snapshot {
            Ok(snapshot) => snapshot,
            Err(error) => {
                let _delivered = request.reply.send(Err(error));
                return;
            }
        };
        request.progress.send_replace(snapshot);
        if request.reply.send(Ok(())).is_err() {
            return;
        }
        self.incoming.remove(&request.id);
        self.used.insert(request.id.clone());
        self.entries.insert(
            request.id,
            Entry {
                progress: request.progress,
                lifetime: request.lifetime,
                work: Work::Idle,
                probe: Probe::Waiting,
            },
        );
        self.tick();
    }
}

#[cfg(test)]
mod tests;
