use rusterisk_media::Frame;
use rusterisk_storage::{Recorder, StorageError, Store};

#[test]
fn stored_files_are_read_as_bounded_exact_bytes_and_truncation_is_an_error(
) -> Result<(), StorageError> {
    let directory = tempfile::tempdir()?;
    let store = Store::open(directory.path())?;
    assert!(matches!(
        store.open_recording("missing"),
        Err(StorageError::NotFound)
    ));
    let mut recording = Recorder::create(directory.path(), "saved", 8000)?;
    for _ in 0..100 {
        recording.write(Frame::new(&[1234; 160], 8000)?)?;
    }
    let path = recording.finish()?;
    let expected = std::fs::read(&path)?;
    let mut reader = store.open_recording("saved")?;
    assert_eq!(reader.length(), expected.len() as u64);
    let mut bytes = Vec::new();
    let mut chunks = 0;
    while let Some(chunk) = reader.read_chunk()? {
        assert!(!chunk.as_ref().is_empty());
        assert!(chunk.as_ref().len() <= 8192);
        bytes.extend_from_slice(chunk.as_ref());
        chunks += 1;
    }
    assert_eq!(chunks, 4);
    assert_eq!(bytes, expected);
    assert!(reader.read_chunk()?.is_none());
    let mut reader = store.open_recording("saved")?;
    std::fs::OpenOptions::new()
        .write(true)
        .open(path)?
        .set_len(44)?;
    assert!(reader.read_chunk().is_err());
    Ok(())
}
