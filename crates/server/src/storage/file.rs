use super::{receive, StoragePort, StoredCommand, WorkerError};
use rusterisk_api::ChannelId;
use rusterisk_storage::{RecordingChunk, StorageError, Store, StoredRecordingReader};
use std::collections::BTreeMap;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

struct Lease {
    lifetime: Option<oneshot::Sender<()>>,
    port: StoragePort,
}

impl Lease {
    fn release(&mut self) {
        drop(self.lifetime.take());
        self.port.wake();
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        self.release();
    }
}

enum State {
    Reading(oneshot::Receiver<Result<(), StorageError>>),
    Complete,
    Failed,
}

pub struct RecordingFile {
    length: u64,
    chunks: mpsc::Receiver<RecordingChunk>,
    state: State,
    lease: Lease,
}

impl RecordingFile {
    pub const fn length(&self) -> u64 {
        self.length
    }

    pub fn into_stream(
        self,
    ) -> impl futures_util::Stream<Item = Result<axum::body::Bytes, WorkerError>> {
        futures_util::stream::try_unfold(self, |mut file| async move {
            Ok(file
                .read()
                .await?
                .map(|chunk| (axum::body::Bytes::copy_from_slice(chunk.as_ref()), file)))
        })
    }

    pub async fn read(&mut self) -> Result<Option<RecordingChunk>, WorkerError> {
        match &self.state {
            State::Complete => return Ok(None),
            State::Failed => return Err(WorkerError::Closed),
            State::Reading(_) => {}
        }
        match tokio::time::timeout(Duration::from_secs(5), self.chunks.recv()).await {
            Ok(Some(chunk)) => {
                self.lease.port.wake();
                Ok(Some(chunk))
            }
            Ok(None) => {
                let State::Reading(completed) = std::mem::replace(&mut self.state, State::Failed)
                else {
                    return Err(WorkerError::Closed);
                };
                let result = receive(completed).await;
                self.lease.release();
                if result.is_ok() {
                    self.state = State::Complete;
                }
                result.map(|()| None)
            }
            Err(_) => {
                self.state = State::Failed;
                self.chunks.close();
                self.lease.release();
                Err(WorkerError::Timeout)
            }
        }
    }
}

impl StoragePort {
    pub async fn recording_file(&self, name: ChannelId) -> Result<RecordingFile, WorkerError> {
        let (output, chunks) = mpsc::channel(3);
        let (lifetime, receiver) = oneshot::channel();
        let (completed, completion) = oneshot::channel();
        let (reply, response) = oneshot::channel();
        let lease = Lease {
            lifetime: Some(lifetime),
            port: self.clone(),
        };
        self.send_stored(StoredCommand::OpenFile {
            name,
            output,
            lifetime: receiver,
            completed,
            reply,
        })?;
        let length = receive(response).await?;
        Ok(RecordingFile {
            length,
            chunks,
            state: State::Reading(completion),
            lease,
        })
    }
}

struct Entry {
    source: StoredRecordingReader,
    output: mpsc::Sender<RecordingChunk>,
    lifetime: oneshot::Receiver<()>,
    completed: oneshot::Sender<Result<(), StorageError>>,
}

pub(super) struct FileSet {
    generation: u64,
    entries: BTreeMap<u64, Entry>,
}

impl FileSet {
    pub fn new() -> Self {
        Self {
            generation: 0,
            entries: BTreeMap::new(),
        }
    }

    pub fn begin(
        &mut self,
        store: &Store,
        name: ChannelId,
        output: mpsc::Sender<RecordingChunk>,
        lifetime: oneshot::Receiver<()>,
        completed: oneshot::Sender<Result<(), StorageError>>,
    ) -> Result<u64, StorageError> {
        if self.entries.len() >= 16 {
            return Err(StorageError::Capacity);
        }
        let generation = self
            .generation
            .checked_add(1)
            .ok_or(StorageError::Capacity)?;
        let source = store.open_recording(name.as_str())?;
        let length = source.length();
        self.entries.insert(
            generation,
            Entry {
                source,
                output,
                lifetime,
                completed,
            },
        );
        self.generation = generation;
        Ok(length)
    }

    pub fn pump(&mut self) {
        let mut finished = Vec::new();
        for (generation, entry) in &mut self.entries {
            let canceled = match entry.lifetime.try_recv() {
                Ok(()) | Err(oneshot::error::TryRecvError::Closed) => true,
                Err(oneshot::error::TryRecvError::Empty) => false,
            };
            if canceled || entry.output.is_closed() || entry.completed.is_closed() {
                finished.push((*generation, Ok(())));
                continue;
            }
            for _ in 0..3 {
                let permit = match entry.output.try_reserve() {
                    Ok(permit) => permit,
                    Err(mpsc::error::TrySendError::Full(())) => break,
                    Err(mpsc::error::TrySendError::Closed(())) => {
                        finished.push((*generation, Ok(())));
                        break;
                    }
                };
                match entry.source.read_chunk() {
                    Ok(Some(chunk)) => permit.send(chunk),
                    Ok(None) => {
                        finished.push((*generation, Ok(())));
                        break;
                    }
                    Err(error) => {
                        finished.push((*generation, Err(error)));
                        break;
                    }
                }
            }
        }
        for (generation, result) in finished {
            if let Some(entry) = self.entries.remove(&generation) {
                if entry.completed.send(result).is_err() {
                    tracing::debug!(generation, "stored file reader disconnected");
                }
            }
        }
    }
}
