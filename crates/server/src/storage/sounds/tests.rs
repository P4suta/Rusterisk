use super::catalogue;
use crate::storage::{drain_commands, Command, FileSet, PlaybackSet, RecordingSet};
use rusterisk_api::ChannelId;
use rusterisk_storage::{CallOutcome, StorageError, Store};
use tokio::sync::{mpsc, oneshot};

#[test]
fn catalogue_yields_to_real_lifecycle_commands_before_finishing_the_library_scan(
) -> Result<(), StorageError> {
    let root = tempfile::tempdir()?;
    std::fs::create_dir_all(root.path().join("sounds/en/digits"))?;
    std::fs::write(root.path().join("sounds/en/digits/readme.txt"), b"fixture")?;
    let mut store = Store::open(root.path())?;
    let mut recordings = RecordingSet::new(root.path().to_owned(), mpsc::channel(1).1);
    let mut playbacks = PlaybackSet::new(root.path().to_owned());
    let mut files = FileSet::new();
    let (commands, mut input) = mpsc::channel(64);
    let (reply, mut result) = oneshot::channel();
    let id = ChannelId::try_from("live-call".to_owned()).map_err(|_| StorageError::InvalidName)?;
    commands
        .try_send(Command::Start {
            id,
            timestamp: "2026-11-16T00:00:00Z".into(),
            reply,
        })
        .map_err(|_| StorageError::Capacity)?;
    let mut observed = false;
    let sounds = catalogue(root.path(), None, None, || {
        drain_commands(
            &mut store,
            &mut recordings,
            &mut playbacks,
            &mut files,
            &mut input,
        );
        if !observed {
            result
                .try_recv()
                .map_err(|_| StorageError::InvalidState)??;
            assert_eq!(store.call_outcome("live-call")?, CallOutcome::Active);
            observed = true;
        }
        Ok(())
    })?;
    assert!(observed);
    assert!(sounds.is_empty());
    Ok(())
}

#[test]
fn cancelled_catalogues_do_not_publish_partial_sound_lists() -> Result<(), StorageError> {
    let root = tempfile::tempdir()?;
    std::fs::create_dir_all(root.path().join("sounds/en"))?;
    let result = catalogue(root.path(), None, None, || {
        Err(StorageError::Io(std::io::Error::new(
            std::io::ErrorKind::Interrupted,
            "fixture cancellation",
        )))
    });
    assert!(
        matches!(result, Err(StorageError::Io(error)) if error.kind() == std::io::ErrorKind::Interrupted)
    );
    Ok(())
}
