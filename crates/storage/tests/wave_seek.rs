use rusterisk_media::Frame;
use rusterisk_storage::{Recorder, StorageError, WaveReader};

#[test]
fn seeking_uses_sample_boundaries_and_preserves_the_next_frame() -> Result<(), StorageError> {
    let directory = tempfile::tempdir()?;
    for rate in [8000, 16000, 48000] {
        let mut recorder = Recorder::create(directory.path(), &format!("seek-{rate}"), rate)?;
        let count = (rate / 50) as usize;
        for value in [1000, 2000, 3000, 4000] {
            recorder.write(Frame::new(&[value; 960][..count], rate)?)?;
        }
        let mut reader = WaveReader::open(&recorder.finish()?)?;
        let count = u64::from(rate / 50);
        assert_eq!(reader.sample_position(), 0);
        assert_eq!(reader.seek_sample(count)?, count);
        let frame = reader.read_frame()?.ok_or(StorageError::InvalidWave)?;
        assert!(frame.samples().iter().all(|sample| *sample == 2000));
        assert_eq!(reader.sample_position(), 2 * count);
        assert_eq!(reader.seek_sample(0)?, 0);
        assert_eq!(
            reader
                .read_frame()?
                .ok_or(StorageError::InvalidWave)?
                .samples()[0],
            1000
        );
        assert_eq!(reader.seek_sample(count - 1)?, count - 1);
        let frame = reader.read_frame()?.ok_or(StorageError::InvalidWave)?;
        assert_eq!(frame.samples()[0], 1000);
        assert!(frame.samples()[1..].iter().all(|sample| *sample == 2000));
        assert_eq!(reader.seek_sample(u64::MAX)?, 4 * count);
        assert!(reader.read_frame()?.is_none());
        assert_eq!(reader.seek_sample(4 * count - 1)?, 4 * count - 1);
        assert_eq!(
            reader
                .read_frame()?
                .ok_or(StorageError::InvalidWave)?
                .samples(),
            &[4000]
        );
        assert!(reader.read_frame()?.is_none());
    }
    Ok(())
}

#[test]
fn seeking_an_empty_recording_stays_at_its_only_valid_sample() -> Result<(), StorageError> {
    let directory = tempfile::tempdir()?;
    let path = Recorder::create(directory.path(), "empty", 8000)?.finish()?;
    let mut reader = WaveReader::open(&path)?;
    for position in [0, 1, u64::MAX] {
        assert_eq!(reader.seek_sample(position)?, 0);
        assert_eq!(reader.sample_position(), 0);
        assert!(reader.read_frame()?.is_none());
    }
    Ok(())
}
