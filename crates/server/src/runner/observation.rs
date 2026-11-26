use super::{timestamp, Controller, Credentials, Request, Runner};
use crate::{CallKey, ControlError};
use rusterisk_api::{
    observation::{WatchEntry, WatchRecord},
    ChannelId,
};
use tokio::sync::{broadcast, oneshot};

pub(crate) enum Target {
    Channel(ChannelId),
    Endpoint {
        technology: String,
        resource: String,
    },
}
pub(crate) enum Filter {
    Channel(CallKey),
    Endpoint(String),
}

impl Filter {
    fn includes(&self, entry: &WatchEntry) -> bool {
        let WatchEntry::Event {
            channel, endpoint, ..
        } = entry
        else {
            return false;
        };
        match self {
            Self::Channel(key) => channel
                .as_ref()
                .is_some_and(|leg| leg.generation == key.generation && leg.id == key.id),
            Self::Endpoint(name) => {
                channel.as_ref().is_some_and(|leg| &leg.endpoint == name)
                    || endpoint
                        .as_ref()
                        .is_some_and(|endpoint| &endpoint.resource == name)
            }
        }
    }
}

pub(crate) struct Observation {
    snapshot: Option<WatchRecord>,
    filter: Filter,
    incoming: broadcast::Receiver<WatchRecord>,
    finished: bool,
}

impl Observation {
    pub(crate) async fn next(&mut self) -> Option<WatchRecord> {
        if let Some(snapshot) = self.snapshot.take() {
            return Some(snapshot);
        }
        if self.finished {
            return None;
        }
        loop {
            match self.incoming.recv().await {
                Ok(record) if self.filter.includes(&record.entry) => {
                    self.finished = matches!(self.filter, Filter::Channel(_))
                        && matches!(&record.entry,WatchEntry::Event{event,..} if event=="ChannelDestroyed");
                    return Some(record);
                }
                Ok(_) => {}
                Err(error) => {
                    self.finished = true;
                    let (reason, skipped) = match error {
                        broadcast::error::RecvError::Lagged(count) => {
                            ("reader_lagged", Some(count))
                        }
                        broadcast::error::RecvError::Closed => ("server_stopped", None),
                    };
                    return Some(WatchRecord {
                        timestamp: timestamp(),
                        entry: WatchEntry::Gap {
                            reason: reason.to_owned(),
                            skipped,
                        },
                    });
                }
            }
        }
    }
}

impl Controller {
    pub(crate) async fn observe(
        &self,
        credentials: Credentials,
        origin: Option<String>,
        target: Target,
    ) -> Result<Observation, ControlError> {
        let (reply, response) = oneshot::channel();
        self.requests
            .try_send(Request::ObserveCalls {
                credentials,
                origin,
                target,
                reply,
            })
            .map_err(ControlError::queue)?;
        self.receive(response).await
    }
}

impl Runner {
    pub(super) fn observe_calls(
        &self,
        credentials: &Credentials,
        origin: Option<&str>,
        target: Target,
    ) -> Result<Observation, ControlError> {
        self.authorize(credentials, origin, true)?;
        if self.observations.receiver_count() >= 64 {
            return Err(ControlError::Capacity);
        }
        let (filter, entry) = self.control.watch_snapshot(target)?;
        Ok(Observation {
            snapshot: Some(WatchRecord {
                timestamp: timestamp(),
                entry,
            }),
            filter,
            incoming: self.observations.subscribe(),
            finished: false,
        })
    }

    pub(super) fn publish_observations(&self, outcome: &crate::Outcome) {
        if self.observations.receiver_count() != 0 {
            self.control.watch_records(outcome, |record| {
                if self.observations.send(record).is_err() {
                    tracing::trace!("Call observation reader disconnected");
                }
            });
        }
    }
}
