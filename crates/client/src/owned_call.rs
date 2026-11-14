use crate::{api, CallCompletion, CallMilestone, Client, Error, OwnedCallProgress};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};
use tokio::sync::{mpsc, oneshot, watch};

#[derive(Clone, Debug)]
pub struct Originate {
    pub endpoint: String,
    pub caller_id: String,
    pub arguments: Vec<String>,
    pub timeout: Duration,
}

impl Originate {
    pub fn new(endpoint: String) -> Result<Self, Error> {
        let value = Self {
            endpoint,
            caller_id: String::new(),
            arguments: Vec::new(),
            timeout: Duration::from_secs(30),
        };
        value.validate()?;
        Ok(value)
    }
    fn validate(&self) -> Result<(), Error> {
        if self.endpoint.is_empty()
            || self.endpoint.len() > 256
            || self.endpoint.chars().any(char::is_control)
            || self.caller_id.len() > 128
            || self.arguments.len() > 32
            || self
                .arguments
                .iter()
                .any(|argument| argument.len() > 256 || argument.contains(','))
            || self.timeout.subsec_nanos() != 0
            || !(1..=3600).contains(&self.timeout.as_secs())
        {
            return Err(Error::Invalid("invalid originate options"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallFailure {
    pub status: Option<u16>,
    pub message: String,
}

impl From<Error> for CallFailure {
    fn from(error: Error) -> Self {
        let status = match &error {
            Error::Api { status, .. } => Some(*status),
            Error::Invalid(_)
            | Error::Id(_)
            | Error::Io(_)
            | Error::Transport(_)
            | Error::Json(_)
            | Error::Capacity
            | Error::Closed
            | Error::Timeout
            | Error::WebSocket(_)
            | Error::EventLagged(_)
            | Error::CallEnded(_)
            | Error::CallFailed(_)
            | Error::Cleanup(_) => None,
        };
        Self {
            status,
            message: error.to_string().chars().take(2048).collect(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Cleanup {
    Pending,
    Complete,
    Failed(String),
}

#[derive(Clone, Debug)]
pub struct CallSnapshot {
    pub progress: OwnedCallProgress,
    pub channel: Option<api::Channel>,
    pub failure: Option<CallFailure>,
    pub cleanup: Cleanup,
}

pub struct OwnedCall {
    id: api::ChannelId,
    progress: watch::Receiver<CallSnapshot>,
    lifetime: Lifetime,
}

enum Lifetime {
    Owned(oneshot::Sender<()>),
    Released(OwnedCallProgress),
}

impl OwnedCall {
    pub fn id(&self) -> &api::ChannelId {
        &self.id
    }
    pub fn snapshot(&self) -> CallSnapshot {
        self.observed(self.progress.borrow().clone())
    }
    fn observed(&self, mut snapshot: CallSnapshot) -> CallSnapshot {
        match &self.lifetime {
            Lifetime::Owned(_) => {}
            Lifetime::Released(progress) => snapshot.progress = *progress,
        }
        snapshot
    }
    pub fn cancel(&mut self) {
        let progress = self
            .progress
            .borrow()
            .progress
            .finish(CallCompletion::Cancelled);
        match std::mem::replace(&mut self.lifetime, Lifetime::Released(progress)) {
            Lifetime::Owned(lifetime) => drop(lifetime),
            Lifetime::Released(previous) => self.lifetime = Lifetime::Released(previous),
        }
    }
    pub async fn wait(
        &mut self,
        milestone: CallMilestone,
        timeout: Duration,
    ) -> Result<CallSnapshot, Error> {
        tokio::time::timeout(timeout, async {
            loop {
                let received = self.progress.borrow_and_update().clone();
                let snapshot = self.observed(received);
                if let Some(completion) = snapshot.progress.completion() {
                    return Err(match completion {
                        CallCompletion::Failed => match snapshot.failure {
                            Some(failure) => Error::CallFailed(failure),
                            None => Error::CallEnded(completion),
                        },
                        CallCompletion::Cancelled | CallCompletion::Ended => {
                            Error::CallEnded(completion)
                        }
                    });
                }
                if snapshot.progress.has_reached(milestone) {
                    return Ok(snapshot);
                }
                self.progress.changed().await.map_err(|_| Error::Closed)?;
            }
        })
        .await
        .map_err(|_| Error::Timeout)?
    }
    pub async fn wait_completion(&mut self, timeout: Duration) -> Result<CallSnapshot, Error> {
        tokio::time::timeout(timeout, async {
            loop {
                let received = self.progress.borrow_and_update().clone();
                let snapshot = self.observed(received);
                if snapshot.progress.completion().is_some() {
                    return Ok(snapshot);
                }
                self.progress.changed().await.map_err(|_| Error::Closed)?;
            }
        })
        .await
        .map_err(|_| Error::Timeout)?
    }
    pub async fn close(mut self) -> Result<(), Error> {
        self.cancel();
        tokio::time::timeout(Duration::from_secs(23), async {
            loop {
                match self.progress.borrow_and_update().cleanup.clone() {
                    Cleanup::Complete => return Ok(()),
                    Cleanup::Failed(message) => return Err(Error::Cleanup(message)),
                    Cleanup::Pending => {}
                }
                self.progress.changed().await.map_err(|_| Error::Closed)?;
            }
        })
        .await
        .map_err(|_| Error::Timeout)?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cancelled_handles_keep_their_result_when_a_late_request_fails() -> Result<(), Error> {
        let (progress, received) = watch::channel(CallSnapshot {
            progress: OwnedCallProgress::new(),
            channel: None,
            failure: None,
            cleanup: Cleanup::Pending,
        });
        let (lifetime, _dropped) = oneshot::channel();
        let mut call = OwnedCall {
            id: api::ChannelId::try_from("cancelled".to_owned())?,
            progress: received,
            lifetime: Lifetime::Owned(lifetime),
        };
        call.cancel();
        progress.send_modify(|snapshot| {
            snapshot.progress = snapshot
                .progress
                .observe(CallMilestone::Answered)
                .finish(CallCompletion::Failed);
            snapshot.failure = Some(CallFailure {
                status: Some(400),
                message: "late request failure".into(),
            });
        });
        assert_eq!(
            call.snapshot().progress.completion(),
            Some(CallCompletion::Cancelled)
        );
        assert!(!call
            .snapshot()
            .progress
            .has_reached(CallMilestone::Answered));
        assert!(matches!(
            call.wait(CallMilestone::Accepted, Duration::from_secs(1))
                .await,
            Err(Error::CallEnded(CallCompletion::Cancelled))
        ));
        Ok(())
    }
}

pub(crate) struct Command {
    id: api::ChannelId,
    options: Originate,
    progress: watch::Sender<CallSnapshot>,
    lifetime: oneshot::Receiver<()>,
    reply: oneshot::Sender<Result<(), Error>>,
}

pub(crate) async fn originate(
    commands: &mpsc::Sender<Command>,
    options: Originate,
) -> Result<OwnedCall, Error> {
    options.validate()?;
    let id = api::ChannelId::try_from(uuid::Uuid::new_v4().to_string())?;
    let (progress, received) = watch::channel(CallSnapshot {
        progress: OwnedCallProgress::new(),
        channel: None,
        failure: None,
        cleanup: Cleanup::Pending,
    });
    let (lifetime, dropped) = oneshot::channel();
    let (reply, accepted) = oneshot::channel();
    commands
        .try_send(Command {
            id: id.clone(),
            options,
            progress,
            lifetime: dropped,
            reply,
        })
        .map_err(|_| Error::Capacity)?;
    accepted.await.map_err(|_| Error::Closed)??;
    Ok(OwnedCall {
        id,
        progress: received,
        lifetime: Lifetime::Owned(lifetime),
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Work {
    Originating,
    EarlyCleaning,
    EarlyCleaned,
    Settling,
    Idle,
    Cleaning,
    Done,
}
struct Entry {
    progress: watch::Sender<CallSnapshot>,
    lifetime: oneshot::Receiver<()>,
    work: Work,
}
pub(crate) struct Calls {
    client: Client,
    application: api::ChannelId,
    entries: BTreeMap<api::ChannelId, Entry>,
    used: BTreeSet<api::ChannelId>,
    pub commands: mpsc::Receiver<Command>,
    pub originates: tokio::task::JoinSet<(api::ChannelId, Result<api::Channel, Error>)>,
    pub cleanups: tokio::task::JoinSet<(api::ChannelId, Result<(), Error>)>,
    cleanup_failure: Option<Error>,
}

impl Calls {
    pub fn new(client: Client, application: api::ChannelId) -> (mpsc::Sender<Command>, Self) {
        let (commands, received) = mpsc::channel(64);
        (
            commands,
            Self {
                client,
                application,
                entries: BTreeMap::new(),
                used: BTreeSet::new(),
                commands: received,
                originates: tokio::task::JoinSet::new(),
                cleanups: tokio::task::JoinSet::new(),
                cleanup_failure: None,
            },
        )
    }

    pub fn submit(&mut self, command: Command) {
        if command.reply.is_closed() {
            return;
        }
        if self.entries.len() >= 64 || self.used.len() >= 4096 || self.used.contains(&command.id) {
            let _delivered = command.reply.send(Err(Error::Capacity));
            return;
        }
        let Command {
            id,
            options,
            progress,
            lifetime,
            reply,
        } = command;
        self.used.insert(id.clone());
        self.entries.insert(
            id.clone(),
            Entry {
                progress,
                lifetime,
                work: Work::Originating,
            },
        );
        let client = self.client.clone();
        let application = self.application.clone();
        self.originates.spawn(async move {
            let timeout = options.timeout.as_secs().to_string();
            let arguments = options.arguments.join(",");
            let mut query: Vec<(&str, &str)> = [
                ("channelId", id.as_str()),
                ("endpoint", options.endpoint.as_str()),
                ("app", application.as_str()),
                ("timeout", timeout.as_str()),
            ]
            .into_iter()
            .collect();
            if !options.caller_id.is_empty() {
                query.push(("callerId", &options.caller_id));
            }
            if !options.arguments.is_empty() {
                query.push(("appArgs", &arguments));
            }
            let result = client.request("channels.originate", &[], &query).await;
            (id, result)
        });
        let _delivered = reply.send(Ok(()));
    }

    pub fn event(&mut self, event: &api::Event) {
        if event.application != self.application.as_str() {
            return;
        }
        let (channel, milestone, completion) = match &event.kind {
            api::EventKind::StasisStart { channel, .. } => {
                (channel, Some(CallMilestone::Stasis), None)
            }
            api::EventKind::ChannelStateChange { channel } => (
                channel,
                if channel.state == api::ChannelState::Up {
                    Some(CallMilestone::Answered)
                } else {
                    None
                },
                None,
            ),
            api::EventKind::ChannelVarset {
                channel,
                variable,
                value,
            } => (
                channel,
                if variable == "RUSTERISK_MEDIA_CONNECTED" && value == "1" {
                    Some(CallMilestone::MediaConnected)
                } else {
                    None
                },
                None,
            ),
            api::EventKind::StasisEnd { channel }
            | api::EventKind::ChannelDestroyed { channel, .. } => {
                (channel, None, Some(CallCompletion::Ended))
            }
            api::EventKind::ChannelCreated { channel }
            | api::EventKind::ChannelCallerId { channel, .. }
            | api::EventKind::ChannelHangupRequest { channel, .. }
            | api::EventKind::ChannelEnteredBridge { channel, .. }
            | api::EventKind::ChannelLeftBridge { channel, .. }
            | api::EventKind::ChannelDtmfReceived { channel, .. }
            | api::EventKind::ChannelHold { channel }
            | api::EventKind::ChannelUnhold { channel }
            | api::EventKind::ChannelUserevent { channel, .. } => (channel, None, None),
            api::EventKind::BridgeCreated { .. }
            | api::EventKind::BridgeDestroyed { .. }
            | api::EventKind::PlaybackStarted { .. }
            | api::EventKind::PlaybackFinished { .. }
            | api::EventKind::RecordingStarted { .. }
            | api::EventKind::RecordingFinished { .. }
            | api::EventKind::RecordingFailed { .. }
            | api::EventKind::ApplicationReplaced { .. }
            | api::EventKind::Dial { .. } => return,
        };
        if let Some(entry) = self.entries.get_mut(&channel.id) {
            entry.progress.send_modify(|snapshot| {
                if snapshot.progress.completion().is_some() {
                    if snapshot.channel.is_none() {
                        snapshot.channel = Some(channel.clone());
                    }
                    return;
                }
                snapshot.channel = Some(channel.clone());
                if channel.state == api::ChannelState::Up {
                    snapshot.progress = snapshot.progress.observe(CallMilestone::Answered);
                }
                if let Some(milestone) = milestone {
                    snapshot.progress = snapshot.progress.observe(milestone);
                }
                if let Some(completion) = completion {
                    snapshot.progress = snapshot.progress.finish(completion);
                }
            });
        }
        self.tick();
    }

    pub fn tick(&mut self) {
        for (id, entry) in &mut self.entries {
            match entry.lifetime.try_recv() {
                Ok(()) | Err(oneshot::error::TryRecvError::Closed) => {
                    entry.progress.send_modify(|snapshot| {
                        snapshot.progress = snapshot.progress.finish(CallCompletion::Cancelled);
                    })
                }
                Err(oneshot::error::TryRecvError::Empty) => {}
            }
            let cleanup = {
                let snapshot = entry.progress.borrow();
                if snapshot.progress.completion().is_none() {
                    false
                } else {
                    match entry.work {
                        Work::Originating if snapshot.channel.is_some() => {
                            entry.work = Work::EarlyCleaning;
                            true
                        }
                        Work::Idle => {
                            entry.work = Work::Cleaning;
                            true
                        }
                        Work::Originating
                        | Work::EarlyCleaning
                        | Work::EarlyCleaned
                        | Work::Settling
                        | Work::Cleaning
                        | Work::Done => false,
                    }
                }
            };
            if cleanup {
                let id = id.clone();
                let client = self.client.clone();
                self.cleanups.spawn(async move {
                    let result = client.hangup(&id).await;
                    (id, result)
                });
            }
        }
        self.entries
            .retain(|_, entry| entry.work != Work::Done || !entry.progress.is_closed());
    }

    pub fn originated(&mut self, id: api::ChannelId, result: Result<api::Channel, Error>) {
        if let Some(entry) = self.entries.get_mut(&id) {
            entry.work = match entry.work {
                Work::Originating | Work::EarlyCleaned => Work::Idle,
                Work::EarlyCleaning => Work::Settling,
                Work::Settling | Work::Idle | Work::Cleaning | Work::Done => return,
            };
            entry.progress.send_modify(|snapshot| match result {
                Ok(channel) if channel.id == id => {
                    snapshot.progress = snapshot.progress.observe(CallMilestone::Accepted);
                    if snapshot.channel.is_none() {
                        snapshot.channel = Some(channel);
                    }
                }
                Ok(_) => {
                    snapshot.progress = snapshot.progress.finish(CallCompletion::Failed);
                    snapshot.failure = Some(CallFailure {
                        status: None,
                        message: "originate returned another channel".into(),
                    });
                }
                Err(error) => {
                    if matches!(
                        error,
                        Error::Api {
                            status: 400..=499 | 501 | 503,
                            ..
                        }
                    ) {
                        if entry.work == Work::Settling {
                            entry.work = Work::Cleaning;
                        } else {
                            entry.work = Work::Done;
                            snapshot.cleanup = Cleanup::Complete;
                        }
                    }
                    snapshot.progress = snapshot.progress.finish(CallCompletion::Failed);
                    snapshot.failure = Some(error.into());
                }
            });
        }
        self.tick();
    }

    pub fn cleaned(&mut self, id: api::ChannelId, result: Result<(), Error>) {
        if let Some(entry) = self.entries.get_mut(&id) {
            match entry.work {
                Work::EarlyCleaning | Work::Settling => {
                    entry.work = if entry.work == Work::EarlyCleaning {
                        Work::EarlyCleaned
                    } else {
                        Work::Idle
                    };
                    if let Err(error) = result {
                        if !matches!(error, Error::Api { status: 404, .. }) {
                            tracing::debug!(channel = %id, %error, "early hangup needs confirmation after originate completes");
                        }
                    }
                    self.tick();
                    return;
                }
                Work::Cleaning => {}
                Work::Originating | Work::EarlyCleaned | Work::Idle | Work::Done => return,
            }
            entry.work = Work::Done;
            entry.progress.send_modify(|snapshot| match result {
                Ok(()) | Err(Error::Api { status: 404, .. }) => {
                    snapshot.cleanup = Cleanup::Complete
                }
                Err(error) => {
                    let message = error.to_string();
                    snapshot.cleanup = Cleanup::Failed(message.clone());
                    if self.cleanup_failure.is_none() {
                        self.cleanup_failure = Some(Error::Cleanup(message));
                    }
                }
            });
        }
        self.tick();
    }

    pub async fn close(&mut self) -> Result<(), Error> {
        self.commands.close();
        for entry in self.entries.values_mut() {
            entry.progress.send_modify(|snapshot| {
                snapshot.progress = snapshot.progress.finish(CallCompletion::Cancelled);
            });
        }
        self.tick();
        let result = tokio::time::timeout(Duration::from_secs(22), async {
            while !self.originates.is_empty() || !self.cleanups.is_empty() {
                tokio::select! {
                    worker = self.originates.join_next(), if !self.originates.is_empty() => {
                        if let Some(worker) = worker { let (id, result) = worker.map_err(|_| Error::Closed)?; self.originated(id, result); }
                    }
                    worker = self.cleanups.join_next(), if !self.cleanups.is_empty() => {
                        if let Some(worker) = worker { let (id, result) = worker.map_err(|_| Error::Closed)?; self.cleaned(id, result); }
                    }
                }
            }
            Ok::<(), Error>(())
        })
        .await;
        self.originates.abort_all();
        self.cleanups.abort_all();
        while self.originates.join_next().await.is_some() {}
        while self.cleanups.join_next().await.is_some() {}
        result.map_err(|_| Error::Timeout)??;
        match self.cleanup_failure.take() {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}
