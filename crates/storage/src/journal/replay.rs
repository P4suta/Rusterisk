use super::{publish_catalog, unlink};
use crate::{
    header, read_header, recover, validate_name, StorageError, WaveReader, MAX_DIRECTORY_ENTRIES,
    MAX_RECORDING,
};
use rusqlite::Connection;
use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::Path;

struct Pending {
    id: i64,
    name: String,
    operation: String,
    phase: String,
    rate: Option<u32>,
    bytes: Option<u32>,
}

fn read_pending(connection: &Connection) -> Result<Vec<Pending>, StorageError> {
    let mut query = connection.prepare("SELECT id,name,operation,phase,sample_rate,final_bytes FROM recording_operations ORDER BY id")?;
    let rows = query
        .query_map([], |row| {
            Ok(Pending {
                id: row.get(0)?,
                name: row.get(1)?,
                operation: row.get(2)?,
                phase: row.get(3)?,
                rate: row.get(4)?,
                bytes: row.get(5)?,
            })
        })?
        .take(MAX_DIRECTORY_ENTRIES + 1)
        .collect::<Result<Vec<_>, _>>()?;
    if rows.len() > MAX_DIRECTORY_ENTRIES {
        return Err(StorageError::Capacity);
    }
    Ok(rows)
}

pub(crate) fn pending(connection: &mut Connection, directory: &Path) -> Result<(), StorageError> {
    for entry in read_pending(connection)? {
        validate_name(&entry.name)?;
        let extension = match entry.operation.as_str() {
            "delete" if entry.phase == "deleting" => {
                unlink(connection, directory, &entry.name, entry.id)?;
                continue;
            }
            "create" => "partial",
            "overwrite" => "replacement",
            "append" => "append",
            _ => return Err(StorageError::InvalidState),
        };
        let stage = directory.join(format!("{}.{extension}", entry.name));
        let outcome = match entry.phase.as_str() {
            "writing" => "interrupted",
            "publishing" => "completed",
            "cancelling" => {
                discard(connection, entry.id, &stage)?;
                continue;
            }
            _ => return Err(StorageError::InvalidState),
        };
        let (rate, bytes) = match (entry.rate, entry.bytes) {
            (Some(rate), Some(bytes)) => (rate, bytes),
            (None, None) if entry.phase == "writing" => {
                let Some((rate, bytes)) = captured(&stage, extension == "append")? else {
                    discard(connection, entry.id, &stage)?;
                    continue;
                };
                connection.execute(
                    "UPDATE recording_operations SET sample_rate=?2,final_bytes=?3 WHERE id=?1",
                    (entry.id, rate, bytes),
                )?;
                (rate, bytes)
            }
            _ => return Err(StorageError::InvalidState),
        };
        publish(&stage, rate, bytes)?;
        publish_catalog(connection, entry.id, outcome)?;
    }
    Ok(())
}

fn discard(connection: &Connection, id: i64, stage: &Path) -> Result<(), StorageError> {
    match std::fs::remove_file(stage) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    sync_parent(stage)?;
    connection.execute("DELETE FROM recording_operations WHERE id=?1", [id])?;
    Ok(())
}

fn captured(stage: &Path, append: bool) -> Result<Option<(u32, u32)>, StorageError> {
    let mut file = match File::open(stage) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let length = file.metadata()?.len();
    if length < 44 {
        return Ok(None);
    }
    if length > MAX_RECORDING + 44 {
        return Err(StorageError::InvalidWave);
    }
    let (rate, prefix) = read_header(&mut file)?;
    let bytes = (length - 44) as u32;
    if append && bytes < prefix {
        return Ok(None);
    }
    Ok(Some((rate, bytes - bytes % 2)))
}

fn publish(stage: &Path, rate: u32, bytes: u32) -> Result<(), StorageError> {
    if !matches!(rate, 8000 | 12000 | 16000 | 24000 | 32000 | 44100 | 48000)
        || u64::from(bytes) > MAX_RECORDING
        || !bytes.is_multiple_of(2)
    {
        return Err(StorageError::InvalidWave);
    }
    if stage.try_exists()? {
        let mut file = OpenOptions::new().read(true).write(true).open(stage)?;
        if file.metadata()?.len() < u64::from(bytes) + 44 {
            return Err(StorageError::InvalidWave);
        }
        file.set_len(u64::from(bytes) + 44)?;
        file.seek(SeekFrom::Start(0))?;
        file.write_all(&header(rate, bytes))?;
        file.sync_all()?;
        drop(file);
        if !recover(stage)? {
            return Err(StorageError::InvalidWave);
        }
    }
    let audio = WaveReader::open(&stage.with_extension("wav"))?;
    if audio.rate() != rate || audio.sample_count() * 2 != u64::from(bytes) {
        return Err(StorageError::InvalidWave);
    }
    sync_parent(stage)
}

fn sync_parent(_path: &Path) -> Result<(), StorageError> {
    #[cfg(unix)]
    File::open(_path.parent().ok_or(StorageError::InvalidState)?)?.sync_all()?;
    Ok(())
}
