use rusterisk_media::Frame;
use rusterisk_storage::{CallOutcome, Recorder, StorageError, Store};

#[test]
fn stored_recording_details_and_deletion_follow_completed_files_without_changing_calls(
) -> Result<(), StorageError> {
    let directory = tempfile::tempdir()?;
    let mut store = Store::open(directory.path())?;
    store.start_call("running", "start")?;
    assert!(matches!(
        store.recording("saved"),
        Err(StorageError::NotFound)
    ));
    assert!(matches!(
        store.delete_recording("saved"),
        Err(StorageError::NotFound)
    ));
    let mut recording = Recorder::create(directory.path(), "saved", 8000)?;
    recording.write(Frame::new(&[1234; 160], 8000)?)?;
    assert!(matches!(
        store.recording("saved"),
        Err(StorageError::NotFound)
    ));
    assert!(store.recordings()?.is_empty());
    let path = recording.finish()?;
    store.recording_finished("saved")?;
    let details = store.recording("saved")?;
    assert_eq!(details.name, "saved");
    assert_eq!(details.format, "wav");
    store.delete_recording("saved")?;
    assert!(!path.exists());
    assert!(store.recordings()?.is_empty());
    assert!(matches!(
        store.recording("saved"),
        Err(StorageError::NotFound)
    ));
    assert!(matches!(
        store.delete_recording("saved"),
        Err(StorageError::NotFound)
    ));
    assert_eq!(store.call_outcome("running")?, CallOutcome::Active);
    for name in ["../outside", "", "/absolute", "name.wav/other"] {
        assert!(matches!(
            store.recording(name),
            Err(StorageError::InvalidName)
        ));
        assert!(matches!(
            store.delete_recording(name),
            Err(StorageError::InvalidName)
        ));
    }
    std::fs::create_dir(directory.path().join("recordings/directory.wav"))?;
    assert!(matches!(
        store.recording("directory"),
        Err(StorageError::NotFound)
    ));
    assert!(matches!(
        store.delete_recording("directory"),
        Err(StorageError::NotFound)
    ));
    assert!(directory.path().join("recordings/directory.wav").is_dir());
    Ok(())
}
