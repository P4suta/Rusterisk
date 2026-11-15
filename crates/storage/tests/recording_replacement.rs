use rusterisk_api::RecordingIfExists;
use rusterisk_media::Frame;
use rusterisk_storage::{Recorder, StorageError, Store, WaveReader};
use std::path::Path;

fn write(root: &Path, sample: i16) -> Result<(), StorageError> {
    let mut recording = Recorder::create(root, "call", 8000)?;
    recording.write(Frame::new(&[sample; 160], 8000)?)?;
    recording.finish()?;
    Ok(())
}
fn samples(root: &Path) -> Result<Frame, StorageError> {
    WaveReader::open(&root.join("recordings/call.wav"))?
        .read_frame()?
        .ok_or(StorageError::InvalidWave)
}

#[test]
fn replacement_reserves_one_directory_slot_and_cannot_overlap_a_create_operation(
) -> Result<(), StorageError> {
    let directory = tempfile::tempdir()?;
    let root = directory.path();
    let active = Recorder::create(root, "call", 8000)?;
    assert!(matches!(
        Recorder::create_with_policy(root, "call", 8000, RecordingIfExists::Overwrite),
        Err(StorageError::Exists)
    ));
    active.cancel()?;
    write(root, 1234)?;
    for index in 0..4094 {
        std::fs::write(root.join("recordings").join(format!("entry-{index}")), [])?;
    }
    let mut replacement =
        Recorder::create_with_policy(root, "call", 8000, RecordingIfExists::Overwrite)?;
    replacement.write(Frame::new(&[6789; 160], 8000)?)?;
    replacement.finish()?;
    let store = Store::open(root)?;
    assert_eq!(samples(root)?.samples(), [6789; 160]);
    assert_eq!(store.recordings()?.len(), 1);
    Ok(())
}

#[test]
fn replacement_preserves_completed_audio_until_publication_and_cancellation_preserves_the_previous_file(
) -> Result<(), StorageError> {
    let directory = tempfile::tempdir()?;
    let root = directory.path();
    write(root, 1234)?;
    let mut previous = WaveReader::open(&root.join("recordings/call.wav"))?;
    let mut replacement =
        Recorder::create_with_policy(root, "call", 8000, RecordingIfExists::Overwrite)?;
    replacement.write(Frame::new(&[5678; 160], 8000)?)?;
    assert_eq!(samples(root)?.samples(), [1234; 160]);
    replacement.finish()?;
    assert_eq!(samples(root)?.samples(), [5678; 160]);
    assert_eq!(
        previous
            .read_frame()?
            .ok_or(StorageError::InvalidWave)?
            .samples(),
        [1234; 160]
    );
    let mut cancelled =
        Recorder::create_with_policy(root, "call", 8000, RecordingIfExists::Overwrite)?;
    cancelled.write(Frame::new(&[9000; 160], 8000)?)?;
    cancelled.cancel()?;
    assert_eq!(samples(root)?.samples(), [5678; 160]);
    assert!(!root.join("recordings/call.replacement").exists());
    assert!(matches!(
        Recorder::create(root, "call", 8000),
        Err(StorageError::Exists)
    ));
    Ok(())
}

#[test]
fn interrupted_replacements_recover_their_samples_over_the_previous_recording(
) -> Result<(), StorageError> {
    let directory = tempfile::tempdir()?;
    let root = directory.path();
    write(root, 1234)?;
    {
        let mut replacement =
            Recorder::create_with_policy(root, "call", 8000, RecordingIfExists::Overwrite)?;
        replacement.write(Frame::new(&[7777; 160], 8000)?)?;
    }
    assert_eq!(samples(root)?.samples(), [1234; 160]);
    let store = Store::open(root)?;
    assert_eq!(samples(root)?.samples(), [7777; 160]);
    assert_eq!(store.recordings()?.len(), 1);
    assert!(!root.join("recordings/call.replacement").exists());
    Ok(())
}

#[test]
fn failed_replacement_publication_keeps_recoverable_audio_and_other_policies_cannot_replace_it(
) -> Result<(), StorageError> {
    let directory = tempfile::tempdir()?;
    let root = directory.path();
    write(root, 1234)?;
    let mut replacement =
        Recorder::create_with_policy(root, "call", 8000, RecordingIfExists::Overwrite)?;
    replacement.write(Frame::new(&[7777; 160], 8000)?)?;
    assert!(matches!(
        Recorder::create_with_policy(root, "call", 8000, RecordingIfExists::Overwrite),
        Err(StorageError::Exists)
    ));
    let path = root.join("recordings/call.wav");
    std::fs::remove_file(&path)?;
    std::fs::create_dir(&path)?;
    assert!(replacement.finish().is_err());
    assert!(root.join("recordings/call.replacement").exists());
    std::fs::remove_dir(&path)?;
    let _store = Store::open(root)?;
    assert_eq!(samples(root)?.samples(), [7777; 160]);
    Ok(())
}
