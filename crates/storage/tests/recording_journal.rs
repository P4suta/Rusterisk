use rusterisk_storage::{StorageError, Store};

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
