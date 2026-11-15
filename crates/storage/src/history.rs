use crate::{validate_name, CallOutcome, StorageError, Store};

#[derive(Clone, Debug)]
pub enum CallHistoryChange {
    Started {
        id: String,
        channel: String,
        generation: u64,
        timestamp: String,
    },
    Finished {
        id: String,
        outcome: CallOutcome,
        timestamp: String,
    },
}

impl CallHistoryChange {
    fn validate(&self) -> Result<(), StorageError> {
        let (id, timestamp) = match self {
            Self::Started {
                id,
                channel,
                timestamp,
                ..
            } => {
                validate_name(channel)?;
                (id, timestamp)
            }
            Self::Finished {
                id,
                outcome,
                timestamp,
            } => {
                if *outcome == CallOutcome::Active {
                    return Err(StorageError::InvalidState);
                }
                (id, timestamp)
            }
        };
        validate_name(id)?;
        if timestamp.len() > 64 {
            return Err(StorageError::Capacity);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallRecord {
    pub id: String,
    pub channel: String,
    pub generation: u64,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub outcome: CallOutcome,
}

impl Store {
    pub fn apply_history(&mut self, changes: &[CallHistoryChange]) -> Result<(), StorageError> {
        if changes.len() > 128 {
            return Err(StorageError::Capacity);
        }
        for change in changes {
            change.validate()?;
        }
        let transaction = self.connection.transaction()?;
        for change in changes {
            match change {
                CallHistoryChange::Started {
                    id,
                    channel,
                    generation,
                    timestamp,
                } => {
                    transaction.execute("INSERT INTO calls (id,channel,generation,started_at,outcome) VALUES (?1,?2,?3,?4,'active')", (id, channel, generation.to_string(), timestamp))?;
                }
                CallHistoryChange::Finished {
                    id,
                    outcome,
                    timestamp,
                } => {
                    let updated = transaction.execute(
                        "UPDATE calls SET outcome=?1, ended_at=?2 WHERE id=?3 AND outcome='active'",
                        (outcome.as_str(), timestamp, id),
                    )?;
                    if updated == 0 {
                        let value: String = transaction.query_row(
                            "SELECT outcome FROM calls WHERE id=?1",
                            [id],
                            |row| row.get(0),
                        )?;
                        if CallOutcome::parse(&value)? != *outcome {
                            return Err(StorageError::InvalidState);
                        }
                    }
                }
            }
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn call_records(&self, limit: u16) -> Result<Vec<CallRecord>, StorageError> {
        if !(1..=4096).contains(&limit) {
            return Err(StorageError::Capacity);
        }
        let mut statement = self.connection.prepare("SELECT id,channel,generation,started_at,ended_at,outcome FROM calls ORDER BY id LIMIT ?1")?;
        let mut rows = statement.query([i64::from(limit)])?;
        let mut records = Vec::new();
        while let Some(row) = rows.next()? {
            let generation: String = row.get(2)?;
            let outcome: String = row.get(5)?;
            records.push(CallRecord {
                id: row.get(0)?,
                channel: row.get(1)?,
                generation: generation.parse().map_err(|_| StorageError::InvalidState)?,
                started_at: row.get(3)?,
                ended_at: row.get(4)?,
                outcome: CallOutcome::parse(&outcome)?,
            });
        }
        Ok(records)
    }
}
