use rusterisk_media::MediaError;
use str0m::rtp::SeqNo;

struct Source {
    id: u32,
    last: u64,
}

pub(super) struct TransmitSources {
    sources: Vec<Source>,
}

impl TransmitSources {
    pub(super) fn new() -> Self {
        Self {
            sources: Vec::with_capacity(2),
        }
    }

    pub(super) fn sequence(&mut self, id: u32, sequence: u16) -> Result<SeqNo, MediaError> {
        if let Some(source) = self.sources.iter_mut().find(|source| source.id == id) {
            let distance = sequence.wrapping_sub(source.last as u16);
            if distance == 0 || distance >= 32768 {
                return Err(MediaError::InvalidPacket);
            }
            source.last = source
                .last
                .checked_add(u64::from(distance))
                .ok_or(MediaError::Capacity)?;
            return Ok(source.last.into());
        }
        if self.sources.len() >= 2 {
            return Err(MediaError::Capacity);
        }
        let last = u64::from(sequence);
        self.sources.push(Source { id, last });
        Ok(last.into())
    }
}

#[cfg(test)]
mod tests {
    use super::TransmitSources;
    use rusterisk_media::MediaError;

    #[test]
    fn source_sequences_extend_wraps_without_aliasing_clock_sources() -> Result<(), MediaError> {
        let mut sources = TransmitSources::new();
        assert_eq!(*sources.sequence(1, 65535)?, 65535);
        assert_eq!(*sources.sequence(2, 41)?, 41);
        assert_eq!(*sources.sequence(1, 0)?, 65536);
        assert_eq!(*sources.sequence(2, 42)?, 42);
        assert_eq!(*sources.sequence(1, 2)?, 65538);
        assert!(matches!(
            sources.sequence(1, 2),
            Err(MediaError::InvalidPacket)
        ));
        assert!(matches!(
            sources.sequence(1, 65535),
            Err(MediaError::InvalidPacket)
        ));
        assert!(matches!(sources.sequence(3, 1), Err(MediaError::Capacity)));
        assert_eq!(*sources.sequence(2, 43)?, 43);
        Ok(())
    }
}
