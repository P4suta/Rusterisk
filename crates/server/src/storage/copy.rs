use super::{receive, respond, StoragePort, StoredCommand, WorkerError};
use rusterisk_api::{ChannelId, StoredRecording};
use rusterisk_storage::{RecordingCopy, StorageError, Store};
use std::collections::VecDeque;
use tokio::sync::oneshot;

struct Entry {
    copy: RecordingCopy,
    destination: ChannelId,
    reply: oneshot::Sender<Result<StoredRecording, StorageError>>,
}

pub(super) struct CopySet {
    entries: VecDeque<Entry>,
}

impl CopySet {
    pub fn new() -> Self {
        Self {
            entries: VecDeque::new(),
        }
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn begin(
        &mut self,
        store: &Store,
        source: ChannelId,
        destination: ChannelId,
        reply: oneshot::Sender<Result<StoredRecording, StorageError>>,
    ) {
        if reply.is_closed() {
            return;
        }
        if self.entries.len() >= 16 {
            respond(reply, Err(StorageError::Capacity));
            return;
        }
        match store.prepare_recording_copy(source.as_str(), destination.as_str()) {
            Ok(copy) => self.entries.push_back(Entry {
                copy,
                destination,
                reply,
            }),
            Err(error) => respond(reply, Err(error)),
        }
    }

    pub fn pump(&mut self) {
        for _ in 0..self.entries.len() {
            let Some(mut entry) = self.entries.pop_front() else {
                break;
            };
            if entry.reply.is_closed() {
                if let Err(error) = entry.copy.cancel() {
                    tracing::warn!(%error, recording = %entry.destination.as_str(), "disconnected recording copy cleanup failed");
                }
                continue;
            }
            match entry.copy.advance() {
                Ok(()) if entry.copy.is_ready() => respond(entry.reply, entry.copy.finish()),
                Ok(()) => self.entries.push_back(entry),
                Err(error) => {
                    if let Err(cleanup) = entry.copy.cancel() {
                        tracing::warn!(%error, %cleanup, recording = %entry.destination.as_str(), "failed recording copy cleanup failed");
                    }
                    respond(entry.reply, Err(error));
                }
            }
        }
    }

    pub fn close(&mut self) {
        while let Some(entry) = self.entries.pop_front() {
            let error = match entry.copy.cancel() {
                Ok(()) => StorageError::InvalidState,
                Err(error) => error,
            };
            respond(entry.reply, Err(error));
        }
    }
}

impl StoragePort {
    pub async fn copy_recording(
        &self,
        source: ChannelId,
        destination: ChannelId,
    ) -> Result<StoredRecording, WorkerError> {
        let (reply, response) = oneshot::channel();
        self.send_stored(StoredCommand::Copy {
            source,
            destination,
            reply,
        })?;
        receive(response).await
    }
}

#[cfg(test)]
mod tests {
    use super::CopySet;
    use rusterisk_api::ChannelId;
    use rusterisk_media::Frame;
    use rusterisk_storage::{Recorder, StorageError, Store};
    use tokio::sync::oneshot;

    #[test]
    fn copies_bound_each_step_and_remove_canceled_or_stopped_stages() -> Result<(), StorageError> {
        let directory = tempfile::tempdir()?;
        let store = Store::open(directory.path())?;
        let mut recording = Recorder::create(directory.path(), "source", 8000)?;
        for _ in 0..100 {
            recording.write(Frame::new(&[1234; 160], 8000)?)?;
        }
        recording.finish()?;
        let source =
            ChannelId::try_from("source".to_owned()).map_err(|_| StorageError::InvalidName)?;
        let mut copies = CopySet::new();
        let mut responses = Vec::new();
        for index in 0..16 {
            let (reply, response) = oneshot::channel();
            copies.begin(
                &store,
                source.clone(),
                ChannelId::try_from(format!("copy-{index}"))
                    .map_err(|_| StorageError::InvalidName)?,
                reply,
            );
            responses.push(response);
        }
        let (reply, mut full) = oneshot::channel();
        copies.begin(
            &store,
            source.clone(),
            ChannelId::try_from("full".to_owned()).map_err(|_| StorageError::InvalidName)?,
            reply,
        );
        assert!(matches!(full.try_recv(), Ok(Err(StorageError::Capacity))));
        copies.pump();
        for index in 0..16 {
            assert_eq!(
                std::fs::metadata(
                    directory
                        .path()
                        .join(format!("recordings/copy-{index}.copying"))
                )?
                .len(),
                8192
            );
            assert!(!directory
                .path()
                .join(format!("recordings/copy-{index}.wav"))
                .exists());
        }
        drop(responses.pop());
        copies.pump();
        assert!(!directory.path().join("recordings/copy-15.copying").exists());
        drop(responses);
        copies.pump();
        assert!(copies.is_empty());
        let (reply, mut stopped) = oneshot::channel();
        copies.begin(
            &store,
            source,
            ChannelId::try_from("stopped".to_owned()).map_err(|_| StorageError::InvalidName)?,
            reply,
        );
        copies.pump();
        copies.close();
        assert!(copies.is_empty());
        assert!(matches!(
            stopped.try_recv(),
            Ok(Err(StorageError::InvalidState))
        ));
        assert!(!directory.path().join("recordings/stopped.copying").exists());
        assert_eq!(store.recordings()?.len(), 1);
        Ok(())
    }
}
