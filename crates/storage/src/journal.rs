use crate::{validate_name, StorageError, Store, MAX_DIRECTORY_ENTRIES};
use rusqlite::Connection;
use std::path::{Path, PathBuf};

pub(super) struct Operation {
    database: PathBuf,
    id: i64,
}

impl Operation {
    pub(super) fn start(
        store: &Store,
        name: &str,
        operation: &str,
        source: Option<&str>,
    ) -> Result<Self, StorageError> {
        let database = store.recordings.with_file_name("calls.sqlite3");
        let id = begin(&store.connection, name, operation, "writing", source)?;
        Ok(Self { database, id })
    }

    fn connect(&self) -> Result<Connection, StorageError> {
        let connection = Connection::open_with_flags(
            &self.database,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE,
        )?;
        connection.busy_timeout(std::time::Duration::from_secs(2))?;
        connection.pragma_update(None, "synchronous", "FULL")?;
        Ok(connection)
    }

    pub(super) fn publishing(&self, rate: u32, bytes: u32) -> Result<(), StorageError> {
        let changed = self.connect()?.execute(
            "UPDATE recording_operations SET phase='publishing',sample_rate=?2,final_bytes=?3 WHERE id=?1 AND phase='writing'",
            (self.id, rate, bytes)
        )?;
        if changed != 1 {
            return Err(StorageError::InvalidState);
        }
        Ok(())
    }

    pub(super) fn cancelling(&self) -> Result<(), StorageError> {
        let changed = self.connect()?.execute(
            "UPDATE recording_operations SET phase='cancelling' WHERE id=?1 AND phase='writing'",
            [self.id],
        )?;
        if changed != 1 {
            return Err(StorageError::InvalidState);
        }
        Ok(())
    }

    pub(super) fn forget(&self) -> Result<(), StorageError> {
        self.connect()?
            .execute("DELETE FROM recording_operations WHERE id=?1", [self.id])?;
        Ok(())
    }

    pub(super) fn completed(&self) -> Result<(), StorageError> {
        publish_catalog(&mut self.connect()?, self.id, "completed")
    }
}

fn publish_catalog(
    connection: &mut Connection,
    id: i64,
    outcome: &str,
) -> Result<(), StorageError> {
    let tx = connection.transaction()?;
    if tx.execute(
        "INSERT INTO recordings (name,format,outcome)
                   SELECT name,'wav',?2 FROM recording_operations WHERE id=?1
                   ON CONFLICT(name) DO UPDATE SET outcome=excluded.outcome",
        (id, outcome),
    )? != 1
    {
        return Err(StorageError::InvalidState);
    }
    tx.execute("DELETE FROM recording_operations WHERE id=?1", [id])?;
    tx.commit()?;
    Ok(())
}

pub(super) fn migrate(connection: &mut Connection) -> Result<(), StorageError> {
    let transaction = connection.transaction()?;
    transaction.execute_batch(
        "CREATE TABLE recording_operations (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL UNIQUE,
            operation TEXT NOT NULL CHECK (operation IN ('create','overwrite','append','copy','delete')),
            phase TEXT NOT NULL CHECK (phase IN ('writing','publishing','cancelling','deleting')),
            source TEXT,
            final_bytes INTEGER CHECK (final_bytes >= 0),
            sample_rate INTEGER
         );
         PRAGMA user_version=3;"
    )?;
    transaction.commit()?;
    Ok(())
}

pub(super) fn begin(
    connection: &Connection,
    name: &str,
    operation: &str,
    phase: &str,
    source: Option<&str>,
) -> Result<i64, StorageError> {
    validate_name(name)?;
    let inserted = connection.execute(
        "INSERT INTO recording_operations (name,operation,phase,source) VALUES (?1,?2,?3,?4)
         ON CONFLICT(name) DO NOTHING",
        (name, operation, phase, source),
    )?;
    if inserted == 0 {
        return Err(StorageError::Exists);
    }
    Ok(connection.last_insert_rowid())
}

pub(super) fn unlink(
    connection: &mut Connection,
    directory: &Path,
    name: &str,
    id: i64,
) -> Result<(), StorageError> {
    validate_name(name)?;
    match std::fs::remove_file(directory.join(format!("{name}.wav"))) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    #[cfg(unix)]
    std::fs::File::open(directory)?.sync_all()?;
    let transaction = connection.transaction()?;
    transaction.execute("DELETE FROM recordings WHERE name=?1", [name])?;
    transaction.execute("DELETE FROM recording_operations WHERE id=?1", [id])?;
    transaction.commit()?;
    Ok(())
}

pub(super) fn replay_deletions(
    connection: &mut Connection,
    directory: &Path,
) -> Result<(), StorageError> {
    let mut query = connection.prepare("SELECT id,name FROM recording_operations WHERE operation='delete' AND phase='deleting' ORDER BY id")?;
    let rows = query
        .query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?
        .take(MAX_DIRECTORY_ENTRIES + 1)
        .collect::<Result<Vec<_>, _>>()?;
    drop(query);
    if rows.len() > MAX_DIRECTORY_ENTRIES {
        return Err(StorageError::Capacity);
    }
    for (id, name) in rows {
        unlink(connection, directory, &name, id)?;
    }
    Ok(())
}
