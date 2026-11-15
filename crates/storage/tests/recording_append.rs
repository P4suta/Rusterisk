use rusterisk_api::RecordingIfExists;
use rusterisk_media::Frame;
use rusterisk_storage::{Recorder, RecordingPreparation, StorageError, Store, WaveReader};
use std::path::Path;

fn original(root: &Path) -> Result<Vec<u8>, StorageError> {
    let mut recorder = Recorder::create(root, "call", 8000)?;
    for _ in 0..100 {
        recorder.write(Frame::new(&[1234; 160], 8000)?)?;
    }
    recorder.finish()?;
    Ok(std::fs::read(root.join("recordings/call.wav"))?)
}

#[test]
fn append_copies_bounded_prefixes_before_accepting_audio_and_cancel_preserves_the_previous_file(
) -> Result<(), StorageError> {
    let directory = tempfile::tempdir()?;
    let root = directory.path();
    let previous = original(root)?;
    let mut preparation =
        RecordingPreparation::create(root, "call", 8000, RecordingIfExists::Append)?;
    assert!(!preparation.is_ready());
    assert!(matches!(
        preparation.write(Frame::new(&[9000; 160], 8000)?),
        Err(StorageError::InvalidState)
    ));
    let mut length = 44;
    let mut steps = 0;
    while !preparation.is_ready() {
        preparation.advance()?;
        let copied = std::fs::metadata(root.join("recordings/call.append"))?.len();
        assert!(copied > length && copied - length <= 8192);
        length = copied;
        steps += 1;
        assert_eq!(std::fs::read(root.join("recordings/call.wav"))?, previous);
    }
    assert_eq!(steps, 4);
    preparation.write(Frame::new(&[5678; 160], 8000)?)?;
    preparation.finish()?;
    let mut reader = WaveReader::open(&root.join("recordings/call.wav"))?;
    for _ in 0..100 {
        assert_eq!(
            reader
                .read_frame()?
                .ok_or(StorageError::InvalidWave)?
                .samples(),
            [1234; 160]
        );
    }
    assert_eq!(
        reader
            .read_frame()?
            .ok_or(StorageError::InvalidWave)?
            .samples(),
        [5678; 160]
    );
    assert!(reader.read_frame()?.is_none());
    let published = std::fs::read(root.join("recordings/call.wav"))?;
    let mut cancelled =
        RecordingPreparation::create(root, "call", 8000, RecordingIfExists::Append)?;
    cancelled.advance()?;
    cancelled.cancel()?;
    assert_eq!(std::fs::read(root.join("recordings/call.wav"))?, published);
    Ok(())
}

#[test]
fn recovery_discards_incomplete_prefixes_and_publishes_complete_appends() -> Result<(), StorageError>
{
    let directory = tempfile::tempdir()?;
    let root = directory.path();
    let previous = original(root)?;
    {
        let mut preparation =
            RecordingPreparation::create(root, "call", 8000, RecordingIfExists::Append)?;
        preparation.advance()?;
    }
    drop(Store::open(root)?);
    assert_eq!(std::fs::read(root.join("recordings/call.wav"))?, previous);
    assert!(!root.join("recordings/call.append").exists());
    {
        let mut preparation =
            RecordingPreparation::create(root, "call", 8000, RecordingIfExists::Append)?;
        while !preparation.is_ready() {
            preparation.advance()?;
        }
        preparation.write(Frame::new(&[6789; 160], 8000)?)?;
    }
    let store = Store::open(root)?;
    let bytes = std::fs::read(root.join("recordings/call.wav"))?;
    assert_eq!(&bytes[44..previous.len()], &previous[44..]);
    assert_eq!(bytes.len(), previous.len() + 320);
    assert_eq!(store.recordings()?.len(), 1);
    Ok(())
}

#[test]
fn append_without_an_existing_file_creates_audio_and_invalid_prefixes_are_rejected_before_staging(
) -> Result<(), StorageError> {
    let directory = tempfile::tempdir()?;
    let root = directory.path();
    let mut recorder = Recorder::create_with_policy(root, "call", 8000, RecordingIfExists::Append)?;
    recorder.write(Frame::new(&[1234; 160], 8000)?)?;
    recorder.finish()?;
    assert!(matches!(
        RecordingPreparation::create(root, "call", 16000, RecordingIfExists::Append),
        Err(StorageError::InvalidWave)
    ));
    assert!(!root.join("recordings/call.append").exists());
    std::fs::write(root.join("recordings/call.wav"), b"invalid wave")?;
    assert!(RecordingPreparation::create(root, "call", 8000, RecordingIfExists::Append).is_err());
    assert!(!root.join("recordings/call.append").exists());
    Ok(())
}
