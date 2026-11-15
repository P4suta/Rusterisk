use super::{
    api, CallMilestone, CallSnapshot, Calls, Error, MAX_FOREGROUND_REQUESTS, MAX_REQUESTS,
};

async fn released(mut progress: tokio::sync::watch::Receiver<CallSnapshot>) {
    loop {
        {
            let snapshot = progress.borrow_and_update();
            if snapshot.progress.completion().is_some()
                || snapshot.progress.has_reached(CallMilestone::MediaConnected)
            {
                return;
            }
        }
        if progress.changed().await.is_err() {
            return;
        }
    }
}

pub(super) enum Probe {
    Waiting,
    Pending,
    Finished,
}

impl Calls {
    pub(super) fn start_probes(&mut self) {
        for (id, entry) in &mut self.entries {
            let snapshot = entry.progress.borrow();
            if snapshot.progress.completion().is_some()
                || snapshot.progress.has_reached(CallMilestone::MediaConnected)
            {
                entry.probe = Probe::Finished;
                continue;
            }
            if !snapshot.progress.has_reached(CallMilestone::Stasis)
                || !matches!(entry.probe, Probe::Waiting)
                || self.originates.len() + self.probes.len() >= MAX_FOREGROUND_REQUESTS
                || self.originates.len() + self.probes.len() + self.cleanups.len() >= MAX_REQUESTS
            {
                continue;
            }
            entry.probe = Probe::Pending;
            let id = id.clone();
            let client = self.client.clone();
            let progress = entry.progress.subscribe();
            self.probes.spawn(async move {
                let result = tokio::select! {
                    biased;
                    () = released(progress) => Err(Error::Closed),
                    result = client.variable(&id, "RUSTERISK_MEDIA_CONNECTED") => result,
                };
                (id, result)
            });
        }
    }

    pub(crate) fn probed(&mut self, id: api::ChannelId, result: Result<api::Variable, Error>) {
        if let Some(entry) = self.entries.get_mut(&id) {
            entry.probe = Probe::Finished;
            entry.progress.send_modify(|snapshot| {
                if snapshot.progress.completion().is_some()
                    || snapshot.progress.has_reached(CallMilestone::MediaConnected)
                {
                    return;
                }
                match result {
                    Ok(variable) => match variable.value.as_str() {
                        "1" => {
                            snapshot.progress =
                                snapshot.progress.observe(CallMilestone::MediaConnected)
                        }
                        "0" => {}
                        _ => {
                            snapshot.media_probe_failure =
                                Some(Error::Invalid("invalid media connection state").into())
                        }
                    },
                    Err(Error::Api { status: 404, .. }) => {}
                    Err(error) => snapshot.media_probe_failure = Some(error.into()),
                }
            });
        }
        self.tick();
    }
}
