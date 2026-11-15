use super::{receive, Command, StoragePort, WorkerError};
use rusterisk_storage::{CallHistoryChange, CallRecord, StorageError};
use tokio::sync::oneshot;

#[must_use]
pub struct HistoryReceipt {
    response: oneshot::Receiver<Result<(), StorageError>>,
}

impl HistoryReceipt {
    pub async fn wait(self) -> Result<(), WorkerError> {
        receive(self.response).await
    }
}

impl StoragePort {
    pub fn queue_history(
        &self,
        changes: Vec<CallHistoryChange>,
    ) -> Result<HistoryReceipt, WorkerError> {
        if changes.len() > 128 {
            return Err(WorkerError::Capacity);
        }
        for change in &changes {
            change.validate()?;
        }
        let (reply, response) = oneshot::channel();
        self.send(Command::History { changes, reply })?;
        Ok(HistoryReceipt { response })
    }

    pub async fn call_records(&self, limit: u16) -> Result<Vec<CallRecord>, WorkerError> {
        if !(1..=4096).contains(&limit) {
            return Err(WorkerError::Invalid);
        }
        let (reply, response) = oneshot::channel();
        self.send(Command::Records { limit, reply })?;
        receive(response).await
    }
}
