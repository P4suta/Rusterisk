use rusterisk_storage::{Recorder, StorageError, Store};

#[test]
fn restart_recovers_every_partial_recording_near_the_directory_limit() -> Result<(), StorageError> {
    let directory = tempfile::tempdir()?;
    let recordings = directory.path().join("recordings");
    std::fs::create_dir(&recordings)?;
    for index in 0..4031 {
        std::fs::write(recordings.join(format!("entry-{index}")), [])?;
    }
    for index in 0..64 {
        let mut recording =
            Recorder::create(directory.path(), &format!("interrupted-{index}"), 8000)?;
        recording.write(rusterisk_media::Frame::new(&[1234; 160], 8000)?)?;
    }
    let store = Store::open(directory.path())?;
    assert_eq!(store.recordings()?.len(), 64);
    for index in 0..64 {
        assert!(!recordings
            .join(format!("interrupted-{index}.partial"))
            .exists());
        let mut reader = rusterisk_storage::WaveReader::open(
            &recordings.join(format!("interrupted-{index}.wav")),
        )?;
        assert_eq!(
            reader
                .read_frame()?
                .ok_or(StorageError::InvalidWave)?
                .samples(),
            [1234; 160]
        );
    }
    Ok(())
}

#[test]
fn recording_admission_reserves_space_for_staging_and_publication_before_creating_files(
) -> Result<(), StorageError> {
    let directory = tempfile::tempdir()?;
    let recordings = directory.path().join("recordings");
    std::fs::create_dir(&recordings)?;
    for index in 0..4095 {
        std::fs::write(recordings.join(format!("entry-{index}")), [])?;
    }
    assert!(matches!(
        Recorder::create(directory.path(), "over-capacity", 8000),
        Err(StorageError::Capacity)
    ));
    assert!(!recordings.join("over-capacity.partial").exists());
    std::fs::remove_file(recordings.join("entry-0"))?;
    Recorder::create(directory.path(), "available", 8000)?.finish()?;
    let store = Store::open(directory.path())?;
    assert_eq!(store.recordings()?.len(), 1);
    Ok(())
}

#[test]
fn listing_counts_every_directory_entry_so_irrelevant_files_cannot_bypass_the_bound(
) -> Result<(), StorageError> {
    let directory = tempfile::tempdir()?;
    let store = Store::open(directory.path())?;
    for index in 0..4097 {
        std::fs::write(
            directory
                .path()
                .join("recordings")
                .join(format!("entry-{index}")),
            [],
        )?;
    }
    assert!(matches!(store.recordings(), Err(StorageError::Capacity)));
    Ok(())
}
