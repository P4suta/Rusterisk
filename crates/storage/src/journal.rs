use crate::StorageError;
use rusqlite::Connection;

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
