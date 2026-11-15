use rusterisk_api::RecordingIfExists;
use rusterisk_media::Frame;
use rusterisk_storage::{Recorder, StorageError, Store};

#[test]
fn recording_copies_publish_exact_complete_bytes_and_reserve_their_destination(
) -> Result<(), StorageError> {
    let directory = tempfile::tempdir()?;
    let store = Store::open(directory.path())?;
    let mut source = Recorder::create(directory.path(), "source", 8000)?;
    for _ in 0..100 {
        source.write(Frame::new(&[1234; 160], 8000)?)?;
    }
    source.finish()?;
    let expected = std::fs::read(directory.path().join("recordings/source.wav"))?;
    let mut copy = store.prepare_recording_copy("source", "copy")?;
    assert!(!copy.is_ready());
    assert!(matches!(
        store.recording("copy"),
        Err(StorageError::NotFound)
    ));
    assert!(matches!(
        store.prepare_recording_copy("source", "copy"),
        Err(StorageError::Exists)
    ));
    assert!(matches!(
        Recorder::create_with_policy(directory.path(), "copy", 8000, RecordingIfExists::Overwrite),
        Err(StorageError::Exists)
    ));
    copy.advance()?;
    assert_eq!(
        std::fs::metadata(directory.path().join("recordings/copy.copying"))?.len(),
        8192
    );
    assert!(!copy.is_ready());
    let mut steps = 1;
    while !copy.is_ready() {
        copy.advance()?;
        steps += 1;
    }
    assert_eq!(steps, 5);
    assert_eq!(copy.finish()?.name, "copy");
    assert_eq!(
        std::fs::read(directory.path().join("recordings/copy.wav"))?,
        expected
    );
    assert!(!directory.path().join("recordings/copy.copying").exists());
    assert!(matches!(
        store.prepare_recording_copy("source", "copy"),
        Err(StorageError::Exists)
    ));
    assert!(matches!(
        store.prepare_recording_copy("missing", "copy"),
        Err(StorageError::NotFound)
    ));
    let mut canceled = store.prepare_recording_copy("source", "cancel")?;
    canceled.advance()?;
    canceled.cancel()?;
    assert!(!directory.path().join("recordings/cancel.copying").exists());
    assert!(matches!(
        store.recording("cancel"),
        Err(StorageError::NotFound)
    ));
    let mut interrupted = store.prepare_recording_copy("source", "interrupted")?;
    interrupted.advance()?;
    drop(interrupted);
    drop(store);
    let reopened = Store::open(directory.path())?;
    assert!(!directory
        .path()
        .join("recordings/interrupted.copying")
        .exists());
    assert!(matches!(
        reopened.recording("interrupted"),
        Err(StorageError::NotFound)
    ));
    assert_eq!(reopened.recordings()?.len(), 2);
    Ok(())
}
