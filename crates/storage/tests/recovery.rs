use rusterisk_media::Frame;
use rusterisk_storage::{CallOutcome, Recorder, Store};

#[test]
fn restart_interrupts_active_calls_and_repairs_recording_headers(
) -> Result<(), rusterisk_storage::StorageError> {
    let directory = tempfile::tempdir()?;
    let root = directory.path();
    {
        let mut store = Store::open(root)?;
        store.start_call("call-a", "2026-11-15T00:00:00Z")?;
        let mut recorder = Recorder::create(root, "interrupted", 8000)?;
        recorder.write(Frame::new(&[1234; 160], 8000)?)?;
    }
    let store = Store::open(root)?;
    assert_eq!(store.call_outcome("call-a")?, CallOutcome::Interrupted);
    let audio = std::fs::read(root.join("recordings/interrupted.wav"))?;
    assert_eq!(audio.len(), 44 + 320);
    assert_eq!(&audio[..4], b"RIFF");
    assert_eq!(&audio[40..44], &320u32.to_le_bytes());
    assert!(!root.join("recordings/interrupted.partial").exists());
    Ok(())
}

#[test]
fn recording_names_cannot_escape_the_recording_directory(
) -> Result<(), rusterisk_storage::StorageError> {
    let directory = tempfile::tempdir()?;
    for name in ["../secret", "x/y", "", ".", ".."] {
        assert!(Recorder::create(directory.path(), name, 8000).is_err());
    }
    Ok(())
}

#[test]
fn completed_audio_and_call_metadata_survive_restart() -> Result<(), rusterisk_storage::StorageError>
{
    let directory = tempfile::tempdir()?;
    {
        let mut store = Store::open(directory.path())?;
        store.start_call("call-b", "start")?;
        store.finish_call("call-b", CallOutcome::Completed, "end")?;
        let mut recorder = Recorder::create(directory.path(), "complete", 8000)?;
        recorder.write(Frame::new(&[999; 160], 8000)?)?;
        recorder.finish()?;
    }
    let store = Store::open(directory.path())?;
    assert_eq!(store.call_outcome("call-b")?, CallOutcome::Completed);
    assert_eq!(store.recordings()?.len(), 1);
    Ok(())
}

#[test]
fn malformed_wave_sizes_and_rates_are_rejected_without_arithmetic_overflow(
) -> Result<(), rusterisk_storage::StorageError> {
    let directory = tempfile::tempdir()?;
    let path = Recorder::create(directory.path(), "malformed", 8000)?.finish()?;
    let original = std::fs::read(&path)?;
    for (offset, value) in [(24, u32::MAX), (40, u32::MAX)] {
        let mut bytes = original.clone();
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        std::fs::write(&path, bytes)?;
        assert!(matches!(
            rusterisk_storage::WaveReader::open(&path),
            Err(rusterisk_storage::StorageError::InvalidWave)
        ));
    }
    Ok(())
}
