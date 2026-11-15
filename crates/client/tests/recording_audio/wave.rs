use rusterisk_client::Error;

fn invalid() -> Error {
    Error::Invalid("invalid recording audio fixture")
}

fn length(bytes: &[u8]) -> Result<usize, Error> {
    usize::try_from(u32::from_le_bytes(bytes.try_into().map_err(|_| invalid())?))
        .map_err(|_| Error::Capacity)
}

pub(super) fn contains(bytes: &[u8], expected: i16) -> Result<bool, Error> {
    if bytes.len() < 44
        || bytes.len() > 1024 * 1024
        || &bytes[..4] != b"RIFF"
        || &bytes[8..12] != b"WAVE"
        || length(&bytes[4..8])?.checked_add(8) != Some(bytes.len())
    {
        return Err(invalid());
    }
    let mut position = 12usize;
    let mut chunks = 0;
    let mut format = None;
    let mut audio = None;
    while position < bytes.len() {
        chunks += 1;
        if chunks > 64 {
            return Err(Error::Capacity);
        }
        let header = bytes.get(position..position + 8).ok_or_else(invalid)?;
        let size = length(&header[4..8])?;
        position += 8;
        let end = position.checked_add(size).ok_or(Error::Capacity)?;
        let contents = bytes.get(position..end).ok_or_else(invalid)?;
        match &header[..4] {
            b"fmt " if format.replace(contents).is_some() => return Err(invalid()),
            b"data" if audio.replace(contents).is_some() => return Err(invalid()),
            _ => {}
        }
        position = end.checked_add(size % 2).ok_or(Error::Capacity)?;
        if position > bytes.len() {
            return Err(invalid());
        }
    }
    let format = format.ok_or_else(invalid)?;
    let audio = audio.ok_or_else(invalid)?;
    if format.get(..16) != Some(b"\x01\0\x01\0\x40\x1f\0\0\x80\x3e\0\0\x02\0\x10\0")
        || audio.len() % 2 != 0
    {
        return Err(invalid());
    }
    let mut consecutive = 0;
    for pair in audio.as_chunks::<2>().0 {
        let sample = i16::from_le_bytes(*pair);
        if (i32::from(sample) - i32::from(expected)).abs() <= 300 {
            consecutive += 1;
            if consecutive >= 640 {
                return Ok(true);
            }
        } else {
            consecutive = 0;
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    fn fixture(sample: i16, extra: bool) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(2048);
        bytes.extend_from_slice(
            b"RIFF\0\0\0\0WAVEfmt \x10\0\0\0\x01\0\x01\0\x40\x1f\0\0\x80\x3e\0\0\x02\0\x10\0",
        );
        if extra {
            bytes.extend_from_slice(b"JUNK\x01\0\0\0x\0");
        }
        bytes.extend_from_slice(b"data\x80\x07\0\0");
        for _ in 0..960 {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
        let size = (bytes.len() - 8) as u32;
        bytes[4..8].copy_from_slice(&size.to_le_bytes());
        bytes
    }

    #[test]
    fn recorded_audio_requires_a_sustained_known_signal_and_accepts_padded_chunks(
    ) -> Result<(), rusterisk_client::Error> {
        for extra in [false, true] {
            assert!(super::contains(&fixture(4092, extra), 4000)?);
            for sample in [0, -7000, 1234] {
                assert!(!super::contains(&fixture(sample, extra), 4000)?);
            }
        }
        let mut transient = fixture(0, false);
        for sample in transient[44..44 + 320].as_chunks_mut::<2>().0 {
            sample.copy_from_slice(&4000i16.to_le_bytes());
        }
        assert!(!super::contains(&transient, 4000)?);
        Ok(())
    }

    #[test]
    fn malformed_or_unsupported_recordings_cannot_satisfy_the_audio_oracle() {
        let original = fixture(4000, false);
        for end in [0, 11, 35, 43, original.len() - 1] {
            assert!(super::contains(&original[..end], 4000).is_err());
        }
        for (position, value) in [(0, b'X'), (8, b'X'), (20, 3), (22, 2), (24, 0), (34, 8)] {
            let mut bytes = original.clone();
            bytes[position] = value;
            assert!(super::contains(&bytes, 4000).is_err());
        }
        let mut oversized = original;
        oversized[40..44].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(super::contains(&oversized, 4000).is_err());
    }
}
