use crate::storage::{HistoryReceipt, StoragePort};
use crate::{CallHistoryEvent, ControlError};
use rusterisk_storage::{CallHistoryChange, CallOutcome};

pub(super) struct HistoryRecorder {
    storage: StoragePort,
    namespace: uuid::Uuid,
}

impl HistoryRecorder {
    pub fn new(storage: StoragePort) -> Result<Self, ControlError> {
        let mut seed = [0; 16];
        getrandom::fill(&mut seed).map_err(|error| ControlError::Adapter(error.to_string()))?;
        Ok(Self {
            storage,
            namespace: uuid::Builder::from_random_bytes(seed).into_uuid(),
        })
    }

    pub fn queue(
        &self,
        events: &mut Vec<CallHistoryEvent>,
    ) -> Result<HistoryReceipt, ControlError> {
        let mut changes = Vec::with_capacity(events.len());
        for event in events.drain(..) {
            changes.push(match event {
                CallHistoryEvent::Started { key, timestamp } => CallHistoryChange::Started {
                    id: format!("{}-{:016x}", self.namespace, key.generation),
                    channel: key.id.to_string(),
                    generation: key.generation,
                    timestamp,
                },
                CallHistoryEvent::Finished { key, timestamp } => CallHistoryChange::Finished {
                    id: format!("{}-{:016x}", self.namespace, key.generation),
                    outcome: CallOutcome::Completed,
                    timestamp,
                },
            });
        }
        self.storage
            .queue_history(changes)
            .map_err(|error| ControlError::Adapter(error.to_string()))
    }
}
