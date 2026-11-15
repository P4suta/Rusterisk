use rusterisk_api::RecordingIfExists;
use rusterisk_media::Frame;
use rusterisk_storage::{Recorder, StorageError, WaveReader};

#[test]
fn tail_trimming_preserves_sample_boundaries_and_updates_the_published_wave_header(
) -> Result<(), StorageError> {
    let directory = tempfile::tempdir()?;
    let mut recorder = Recorder::create(directory.path(), "trimmed", 8000)?;
    recorder.write(Frame::new(&[1234; 160], 8000)?)?;
    recorder.write(Frame::new(&[0; 160], 8000)?)?;
    let path = recorder.finish_with_tail_trim(10)?;
    assert_eq!(std::fs::metadata(&path)?.len(), 44 + 480);
    let mut reader = WaveReader::open(&path)?;
    assert_eq!(
        reader
            .read_frame()?
            .ok_or(StorageError::InvalidWave)?
            .samples(),
        [1234; 160]
    );
    assert_eq!(
        reader
            .read_frame()?
            .ok_or(StorageError::InvalidWave)?
            .samples(),
        [0; 80]
    );
    assert!(reader.read_frame()?.is_none());
    Ok(())
}

#[test]
fn tail_trimming_cannot_remove_the_preserved_prefix_of_an_append() -> Result<(), StorageError> {
    let directory = tempfile::tempdir()?;
    let mut original = Recorder::create(directory.path(), "call", 8000)?;
    original.write(Frame::new(&[5678; 160], 8000)?)?;
    let path = original.finish()?;
    let previous = std::fs::read(&path)?;
    let mut append =
        Recorder::create_with_policy(directory.path(), "call", 8000, RecordingIfExists::Append)?;
    append.write(Frame::new(&[0; 160], 8000)?)?;
    append.finish_with_tail_trim(u64::MAX)?;
    assert_eq!(std::fs::read(&path)?, previous);
    let mut replacement =
        Recorder::create_with_policy(directory.path(), "call", 8000, RecordingIfExists::Overwrite)?;
    replacement.write(Frame::new(&[0; 160], 8000)?)?;
    replacement.finish_with_tail_trim(u64::MAX)?;
    assert_eq!(std::fs::metadata(path)?.len(), 44);
    Ok(())
}
