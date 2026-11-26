use rusterisk_storage::{StorageError, Store};

#[test]
fn interrupted_append_publication_preserves_the_trimmed_audio_and_never_repeats_the_prefix(
) -> Result<(), StorageError> {
    use rusterisk_api::RecordingIfExists::{Append, Fail};
    let directory = tempfile::tempdir()?;
    let root = directory.path();
    let store = Store::open(root)?;
    let db = rusqlite::Connection::open(root.join("calls.sqlite3"))?;
    let mut first = store.prepare_recording("clip", 8000, Fail)?;
    first.write(rusterisk_media::Frame::new(&[111; 160], 8000)?)?;
    first.finish()?;
    let mut append = store.prepare_recording("clip", 8000, Append)?;
    while !append.is_ready() {
        append.advance()?;
    }
    for sample in [222, 333] {
        append.write(rusterisk_media::Frame::new(&[sample; 160], 8000)?)?;
    }
    db.execute_batch("CREATE TRIGGER interrupted_catalog BEFORE UPDATE ON recordings BEGIN SELECT RAISE(ABORT,'interrupted catalog'); END;")?;
    assert!(append.finish_with_tail_trim(20).is_err());
    assert_eq!(
        db.query_row(
            "SELECT final_bytes FROM recording_operations WHERE name='clip' AND phase='publishing'",
            [],
            |row| row.get::<_, u32>(0)
        )?,
        640
    );
    db.execute_batch("DROP TRIGGER interrupted_catalog")?;
    drop(store);
    for _ in 0..2 {
        let _store = Store::open(root)?;
        assert_eq!(
            db.query_row("SELECT count(*) FROM recording_operations", [], |row| row
                .get::<_, u32>(
                0
            ))?,
            0
        );
        let mut audio = rusterisk_storage::WaveReader::open(&root.join("recordings/clip.wav"))?;
        for sample in [111, 222] {
            assert_eq!(
                audio
                    .read_frame()?
                    .ok_or(StorageError::InvalidWave)?
                    .samples(),
                &[sample; 160]
            );
        }
        assert!(audio.read_frame()?.is_none());
    }
    Ok(())
}

#[test]
fn recovery_discards_cancelled_stages_and_marks_only_interrupted_capture(
) -> Result<(), StorageError> {
    use rusterisk_api::RecordingIfExists::Fail;
    let directory = tempfile::tempdir()?;
    let root = directory.path();
    let store = Store::open(root)?;
    let db = rusqlite::Connection::open(root.join("calls.sqlite3"))?;
    for name in ["cancelled", "captured"] {
        let mut active = store.prepare_recording(name, 8000, Fail)?;
        active.write(rusterisk_media::Frame::new(&[321; 160], 8000)?)?;
    }
    db.execute(
        "UPDATE recording_operations SET phase='cancelling' WHERE name='cancelled'",
        [],
    )?;
    drop(store);
    for _ in 0..2 {
        let store = Store::open(root)?;
        assert!(matches!(
            store.recording("cancelled"),
            Err(StorageError::NotFound)
        ));
        assert_eq!(
            db.query_row(
                "SELECT outcome FROM recordings WHERE name='captured'",
                [],
                |row| row.get::<_, String>(0)
            )?,
            "interrupted"
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM recording_operations", [], |row| row
                .get::<_, u32>(
                0
            ))?,
            0
        );
    }
    Ok(())
}
