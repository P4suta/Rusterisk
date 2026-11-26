use crate::{validate_name, StorageError, MAX_DIRECTORY_ENTRIES};
use rusqlite::Connection;
use std::path::Path;

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
