use rusterisk_storage::{StorageError, Store};

#[test]
fn admitted_recordings_journal_the_policy_and_commit_the_catalog_before_reporting_completion(
) -> Result<(), StorageError> {
    use rusterisk_api::RecordingIfExists::{Append, Fail, Overwrite};
    let directory = tempfile::tempdir()?;
    let root = directory.path();
    let store = Store::open(root)?;
    let db = rusqlite::Connection::open(root.join("calls.sqlite3"))?;
    for (policy, operation, sample) in [
        (Fail, "create", 11),
        (Overwrite, "overwrite", 22),
        (Append, "append", 33),
    ] {
        let mut recording = store.prepare_recording("clip", 8000, policy)?;
        let pending: (String, String) = db.query_row(
            "SELECT operation,phase FROM recording_operations WHERE name='clip'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert_eq!(pending, (operation.into(), "writing".into()));
        while !recording.is_ready() {
            recording.advance()?;
        }
        recording.write(rusterisk_media::Frame::new(&[sample; 160], 8000)?)?;
        recording.finish()?;
        assert_eq!(
            db.query_row(
                "SELECT outcome FROM recordings WHERE name='clip'",
                [],
                |row| row.get::<_, String>(0)
            )?,
            "completed"
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM recording_operations", [], |row| row
                .get::<_, u32>(
                0
            ))?,
            0
        );
    }
    let mut reader = rusterisk_storage::WaveReader::open(&root.join("recordings/clip.wav"))?;
    for sample in [22, 33] {
        assert_eq!(
            reader
                .read_frame()?
                .ok_or(StorageError::InvalidWave)?
                .samples(),
            &[sample; 160]
        );
    }
    assert!(reader.read_frame()?.is_none());
    store.prepare_recording("clip", 8000, Overwrite)?.cancel()?;
    assert_eq!(
        db.query_row("SELECT count(*) FROM recording_operations", [], |row| row
            .get::<_, u32>(
            0
        ))?,
        0
    );
    Ok(())
}

#[test]
fn deletion_replays_before_and_after_unlink_and_keeps_its_intent_until_catalog_commit(
) -> Result<(), StorageError> {
    let directory = tempfile::tempdir()?;
    let root = directory.path();
    let mut store = Store::open(root)?;
    let db = rusqlite::Connection::open(root.join("calls.sqlite3"))?;
    for name in ["before", "after", "keep"] {
        rusterisk_storage::Recorder::create(root, name, 8000)?.finish()?;
        store.recording_finished(name)?;
    }
    db.execute_batch(
        "CREATE TRIGGER interrupted_delete BEFORE DELETE ON recordings
        WHEN OLD.name='after' BEGIN SELECT RAISE(ABORT,'interrupted catalog write'); END;",
    )?;
    assert!(store.delete_recording("after").is_err());
    assert!(!root.join("recordings/after.wav").exists());
    let pending: u32 = db.query_row("SELECT count(*) FROM recording_operations WHERE name='after' AND operation='delete' AND phase='deleting'", [], |row| row.get(0))?;
    assert_eq!(pending, 1);
    db.execute_batch("DROP TRIGGER interrupted_delete;
        INSERT INTO recording_operations(name,operation,phase) VALUES ('before','delete','deleting');")?;
    drop(store);
    for _ in 0..2 {
        let reopened = Store::open(root)?;
        assert_eq!(
            reopened
                .recordings()?
                .iter()
                .map(|r| r.name.as_str())
                .collect::<Vec<_>>(),
            ["keep"]
        );
        for table in ["recordings", "recording_operations"] {
            assert_eq!(
                db.query_row(
                    &format!("SELECT count(*) FROM {table} WHERE name IN ('before','after')"),
                    [],
                    |row| row.get::<_, u32>(0)
                )?,
                0
            );
        }
    }
    Ok(())
}

#[test]
fn upgrading_a_stopped_store_preserves_history_and_installs_the_recording_journal(
) -> Result<(), StorageError> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("calls.sqlite3");
    let database = rusqlite::Connection::open(&path)?;
    database.execute_batch(
        "CREATE TABLE calls (id TEXT PRIMARY KEY, channel TEXT, generation TEXT, started_at TEXT, ended_at TEXT, outcome TEXT);
         CREATE TABLE recordings (name TEXT PRIMARY KEY, format TEXT, outcome TEXT);
         INSERT INTO calls VALUES ('old', 'phone', '7', 'start', 'end', 'completed');
         PRAGMA user_version=2;"
    )?;
    drop(database);
    for _ in 0..2 {
        let store = Store::open(directory.path())?;
        assert_eq!(store.call_records(1)?[0].generation, 7);
        let database = rusqlite::Connection::open(&path)?;
        assert_eq!(
            database.pragma_query_value(None, "user_version", |row| row.get::<_, u32>(0))?,
            3
        );
        assert_eq!(
            database.query_row("SELECT count(*) FROM recording_operations", [], |row| row
                .get::<_, u32>(
                0
            ))?,
            0
        );
    }
    Ok(())
}
