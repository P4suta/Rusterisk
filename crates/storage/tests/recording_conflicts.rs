use rusterisk_media::Frame;
use rusterisk_storage::{Recorder, StorageError, WaveReader};

#[test]
fn existing_partial_and_completed_recordings_have_typed_conflicts_without_changing_audio(
) -> Result<(), StorageError> {
    let directory = tempfile::tempdir()?;
    let mut recorder = Recorder::create(directory.path(), "call", 8000)?;
    assert!(matches!(
        Recorder::create(directory.path(), "call", 8000),
        Err(StorageError::Exists)
    ));
    recorder.write(Frame::new(&[1234; 160], 8000)?)?;
    let path = recorder.finish()?;
    assert!(matches!(
        Recorder::create(directory.path(), "call", 8000),
        Err(StorageError::Exists)
    ));
    assert!(matches!(
        Recorder::create(directory.path(), "../call", 8000),
        Err(StorageError::InvalidName)
    ));
    let mut reader = WaveReader::open(&path)?;
    assert_eq!(
        reader
            .read_frame()?
            .ok_or(StorageError::InvalidWave)?
            .samples(),
        [1234; 160]
    );
    assert!(reader.read_frame()?.is_none());
    Ok(())
}
