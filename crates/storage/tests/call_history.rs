use rusterisk_storage::{CallHistoryChange, CallOutcome, StorageError, Store};

fn start(id: &str, generation: u64) -> CallHistoryChange {
    CallHistoryChange::Started {
        id: id.into(),
        channel: "reused".into(),
        generation,
        timestamp: "start".into(),
    }
}

#[test]
fn history_preserves_channel_identity_across_sessions_and_commits_each_batch_atomically(
) -> Result<(), StorageError> {
    let directory = tempfile::tempdir()?;
    let mut store = Store::open(directory.path())?;
    store.apply_history(&[
        start("first-session-1", 1),
        CallHistoryChange::Finished {
            id: "first-session-1".into(),
            outcome: CallOutcome::Completed,
            timestamp: "end".into(),
        },
        start("second-session-1", u64::MAX),
    ])?;
    let records = store.call_records(10)?;
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].channel, "reused");
    assert_eq!(records[0].generation, 1);
    assert_eq!(records[0].outcome, CallOutcome::Completed);
    assert_eq!(records[0].ended_at.as_deref(), Some("end"));
    assert_eq!(records[1].generation, u64::MAX);
    assert_eq!(records[1].outcome, CallOutcome::Active);
    assert!(store
        .apply_history(&[start("rolled-back", 3), start("first-session-1", 4)])
        .is_err());
    assert_eq!(store.call_records(10)?.len(), 2);
    drop(store);
    let restarted = Store::open(directory.path())?;
    assert_eq!(
        restarted.call_outcome("second-session-1")?,
        CallOutcome::Interrupted
    );
    assert_eq!(
        restarted.call_outcome("first-session-1")?,
        CallOutcome::Completed
    );
    Ok(())
}

#[test]
fn invalid_history_is_rejected_before_committing_any_changes() -> Result<(), StorageError> {
    let directory = tempfile::tempdir()?;
    let mut store = Store::open(directory.path())?;
    assert!(store
        .apply_history(&[
            start("not-created", 1),
            CallHistoryChange::Finished {
                id: "not-created".into(),
                outcome: CallOutcome::Active,
                timestamp: "end".into()
            },
        ])
        .is_err());
    assert!(store.call_records(10)?.is_empty());
    assert!(store.call_records(0).is_err());
    assert!(store.call_records(4097).is_err());
    assert!(store
        .apply_history(
            &(0..129)
                .map(|generation| start(&format!("call-{generation}"), generation))
                .collect::<Vec<_>>()
        )
        .is_err());
    assert!(store.call_records(10)?.is_empty());
    Ok(())
}

#[test]
fn existing_history_is_migrated_before_recovery() -> Result<(), StorageError> {
    let directory = tempfile::tempdir()?;
    let connection = rusqlite::Connection::open(directory.path().join("calls.sqlite3"))?;
    connection.execute_batch("CREATE TABLE calls (id TEXT PRIMARY KEY NOT NULL, started_at TEXT NOT NULL, ended_at TEXT, outcome TEXT NOT NULL CHECK (outcome IN ('active','completed','interrupted'))); CREATE TABLE recordings (name TEXT PRIMARY KEY NOT NULL, format TEXT NOT NULL, outcome TEXT NOT NULL); INSERT INTO calls VALUES ('legacy', 'start', NULL, 'active'); PRAGMA user_version=1;")?;
    drop(connection);
    let store = Store::open(directory.path())?;
    let records = store.call_records(10)?;
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].id, "legacy");
    assert_eq!(records[0].channel, "legacy");
    assert_eq!(records[0].generation, 0);
    assert_eq!(records[0].outcome, CallOutcome::Interrupted);
    assert!(records[0].ended_at.is_some());
    Ok(())
}
