use rusterisk_media::MediaError;
use str0m::rtp::SeqNo;

struct Source {
    id: u32,
    last: Option<u64>,
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
        self.register(id)?;
        let source = self
            .sources
            .iter_mut()
            .find(|source| source.id == id)
            .ok_or(MediaError::InvalidRoute)?;
        let number = if let Some(last) = source.last {
            let distance = sequence.wrapping_sub(last as u16);
            if distance == 0 || distance >= 32768 {
                return Err(MediaError::InvalidPacket);
            }
            last.checked_add(u64::from(distance))
                .ok_or(MediaError::Capacity)?
        } else {
            u64::from(sequence)
        };
        source.last = Some(number);
        Ok(number.into())
    }

    pub(super) fn contains(&self, id: u32) -> bool {
        self.sources.iter().any(|source| source.id == id)
    }

    pub(super) fn register(&mut self, id: u32) -> Result<(), MediaError> {
        if self.contains(id) {
            return Ok(());
        }
        if self.sources.len() >= 2 {
            return Err(MediaError::Capacity);
        }
        self.sources.push(Source { id, last: None });
        Ok(())
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
