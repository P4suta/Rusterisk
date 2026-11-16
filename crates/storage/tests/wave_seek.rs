use rusterisk_media::Frame;
use rusterisk_storage::{Recorder, StorageError, WaveReader};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

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

#[test]
fn owned_files_reset_the_cursor_and_preserve_known_audio_at_each_supported_rate(
) -> Result<(), StorageError> {
    owned_fixture(|root| {
        for rate in [8000, 12000, 16000, 24000, 32000, 44100, 48000] {
            let count = (rate / 50) as usize;
            let mut recorder = Recorder::create(root, &format!("owned-{rate}"), rate)?;
            for sample in [1234, -2345] {
                recorder.write(Frame::new(&[sample; 960][..count], rate)?)?;
            }
            recorder.write(Frame::new(&[3456; 3], rate)?)?;
            let path = recorder.finish()?;
            let samples = 2 * count as u64 + 3;
            let length = 44 + 2 * samples;
            for cursor in [0, 7, 44, length + 9] {
                let mut file = File::open(&path)?;
                file.seek(SeekFrom::Start(cursor))?;
                let mut reader = WaveReader::from_file(file)?;
                verify(
                    reader.rate() == rate,
                    "owned reader changed the sample rate",
                )?;
                verify(
                    reader.sample_count() == samples,
                    "owned reader changed the sample count",
                )?;
                verify(
                    reader.sample_position() == 0,
                    "owned reader kept the caller cursor",
                )?;
                verify(
                    reader.metadata()?.len() == length,
                    "owned metadata changed the file length",
                )?;
                for sample in [1234, -2345] {
                    let frame = reader.read_frame()?.ok_or(StorageError::InvalidWave)?;
                    verify(
                        frame.samples() == &[sample; 960][..count],
                        "owned reader changed known full-frame audio",
                    )?;
                }
                let frame = reader.read_frame()?.ok_or(StorageError::InvalidWave)?;
                verify(
                    frame.samples() == [3456; 3],
                    "owned reader changed the partial frame",
                )?;
                verify(
                    reader.read_frame()?.is_none(),
                    "owned reader exceeded the declared audio",
                )?;
                verify(
                    reader.sample_position() == samples,
                    "owned reader lost the consumed position",
                )?;
            }
        }
        Ok(())
    })
}

#[test]
fn owned_files_reject_invalid_canonical_pcm_headers() -> Result<(), StorageError> {
    owned_fixture(|root| {
        let path = wave(root, "header-source", 1234, 1)?;
        let mut header = [0u8; 44];
        File::open(&path)?.read_exact(&mut header)?;
        let mut headers = [header; 7];
        headers[0][0] = b'X';
        headers[1][20..22].copy_from_slice(&3u16.to_le_bytes());
        headers[2][22..24].copy_from_slice(&2u16.to_le_bytes());
        headers[3][34..36].copy_from_slice(&8u16.to_le_bytes());
        headers[4][28..32].copy_from_slice(&1u32.to_le_bytes());
        headers[5][32..34].copy_from_slice(&4u16.to_le_bytes());
        headers[6][24..28].copy_from_slice(&11025u32.to_le_bytes());
        for (index, header) in headers.into_iter().enumerate() {
            let invalid = root.join(format!("invalid-header-{index}.wav"));
            std::fs::copy(&path, &invalid)?;
            let mut file = OpenOptions::new().read(true).write(true).open(invalid)?;
            file.write_all(&header)?;
            let result = WaveReader::from_file(file);
            verify(
                matches!(result, Err(StorageError::InvalidWave)),
                format!("owned reader accepted invalid header {index}"),
            )?;
        }
        Ok(())
    })
}

#[test]
fn owned_files_reject_short_headers_and_declared_length_mismatches() -> Result<(), StorageError> {
    owned_fixture(|root| {
        let source = wave(root, "length-source", 1234, 1)?;
        let length = std::fs::metadata(&source)?.len();
        let short = root.join("short-header.wav");
        std::fs::copy(&source, &short)?;
        let file = OpenOptions::new().read(true).write(true).open(short)?;
        file.set_len(43)?;
        verify(
            matches!(WaveReader::from_file(file), Err(StorageError::Io(error)) if error.kind() == std::io::ErrorKind::UnexpectedEof),
            "owned reader hid a short header",
        )?;
        for actual in [length - 2, length + 1] {
            let path = root.join(format!("mismatched-length-{actual}.wav"));
            std::fs::copy(&source, &path)?;
            let file = OpenOptions::new().read(true).write(true).open(path)?;
            file.set_len(actual)?;
            verify(
                matches!(WaveReader::from_file(file), Err(StorageError::InvalidWave)),
                "owned reader accepted a declared length mismatch",
            )?;
        }
        Ok(())
    })
}

#[test]
fn owned_metadata_reports_truncation_and_audio_reads_preserve_the_failure(
) -> Result<(), StorageError> {
    owned_fixture(|root| {
        let path = wave(root, "truncated", 1234, 1)?;
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        let truncator = file.try_clone()?;
        let mut reader = WaveReader::from_file(file)?;
        truncator.set_len(46)?;
        verify(
            reader.metadata()?.len() == 46,
            "owned metadata cached the original file length",
        )?;
        verify(
            matches!(reader.read_frame(), Err(StorageError::Io(error)) if error.kind() == std::io::ErrorKind::UnexpectedEof),
            "owned reader hid post-admission truncation",
        )?;
        Ok(())
    })
}

#[test]
fn owned_metadata_and_audio_stay_with_the_handle_after_path_replacement() -> Result<(), StorageError>
{
    owned_fixture(|root| {
        let source = wave(root, "path-source", 1234, 1)?;
        let replacement = wave(root, "path-replacement", -2345, 2)?;
        let file = File::open(&source)?;
        let original_length = file.metadata()?.len();
        std::fs::rename(&source, root.join("retained-source.wav"))?;
        std::fs::rename(replacement, &source)?;
        let mut reader = WaveReader::from_file(file)?;
        verify(
            reader.metadata()?.len() == original_length,
            "owned metadata followed the replacement pathname",
        )?;
        verify(
            std::fs::metadata(&source)?.len() > original_length,
            "replacement fixture did not change the pathname metadata",
        )?;
        let frame = reader.read_frame()?.ok_or(StorageError::InvalidWave)?;
        verify(
            frame.samples() == [1234; 160],
            "owned reader followed replacement audio",
        )?;
        verify(
            reader.read_frame()?.is_none(),
            "owned reader followed the replacement length",
        )?;
        Ok(())
    })
}

#[test]
fn owned_empty_files_preserve_the_existing_zero_sample_contract() -> Result<(), StorageError> {
    owned_fixture(|root| {
        let path = Recorder::create(root, "owned-empty", 8000)?.finish()?;
        let mut reader = WaveReader::from_file(File::open(path)?)?;
        verify(
            reader.metadata()?.len() == 44,
            "empty owned metadata changed the header length",
        )?;
        verify(
            reader.sample_count() == 0,
            "owned reader rejected empty recording semantics",
        )?;
        verify(
            reader.read_frame()?.is_none(),
            "owned reader invented empty recording audio",
        )?;
        Ok(())
    })
}

fn wave(root: &Path, name: &str, sample: i16, frames: usize) -> Result<PathBuf, StorageError> {
    let mut recorder = Recorder::create(root, name, 8000)?;
    for _ in 0..frames {
        recorder.write(Frame::new(&[sample; 160], 8000)?)?;
    }
    recorder.finish()
}

fn verify(condition: bool, message: impl Into<String>) -> Result<(), StorageError> {
    if condition {
        Ok(())
    } else {
        Err(StorageError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            message.into(),
        )))
    }
}

fn owned_fixture(
    check: impl FnOnce(&Path) -> Result<(), StorageError>,
) -> Result<(), StorageError> {
    let directory = tempfile::tempdir()?;
    let result = check(directory.path());
    let cleanup = directory.close().map_err(StorageError::from);
    match (result, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(error), Err(cleanup)) => Err(StorageError::Io(std::io::Error::other(format!(
            "{error}; owned wave fixture cleanup failed: {cleanup}"
        )))),
    }
}
