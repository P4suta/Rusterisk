use crate::{MediaError, RtpPacket};
use rusterisk_core::{DtmfDigit, DtmfTone};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TelephoneEvent {
    digit: DtmfDigit,
    ended: bool,
    volume: u8,
    duration: u16,
}

impl TelephoneEvent {
    pub fn new(
        digit: DtmfDigit,
        ended: bool,
        volume: u8,
        duration: u16,
    ) -> Result<Self, MediaError> {
        if volume > 63 {
            return Err(MediaError::InvalidPacket);
        }
        Ok(Self {
            digit,
            ended,
            volume,
            duration,
        })
    }

    pub fn parse(bytes: &[u8]) -> Result<Self, MediaError> {
        let [digit, flags, high, low] = bytes else {
            return Err(MediaError::InvalidPacket);
        };
        Self::new(
            DtmfDigit::from_event(*digit).ok_or(MediaError::Unsupported)?,
            flags & 0x80 != 0,
            flags & 0x3f,
            u16::from_be_bytes([*high, *low]),
        )
    }

    pub const fn encode(self) -> [u8; 4] {
        let [high, low] = self.duration.to_be_bytes();
        [
            self.digit.event(),
            self.volume | if self.ended { 0x80 } else { 0 },
            high,
            low,
        ]
    }

    pub const fn digit(self) -> DtmfDigit {
        self.digit
    }
    pub const fn ended(self) -> bool {
        self.ended
    }
    pub const fn duration(self) -> u16 {
        self.duration
    }
}

#[derive(Clone, Copy)]
struct Reception {
    ssrc: u32,
    digit: DtmfDigit,
    start: u32,
    segment: u32,
    prior: u64,
    duration: u16,
    sequence: u16,
    updated: u64,
}

impl Reception {
    fn tone(self, rate: u32) -> Result<DtmfTone, MediaError> {
        let duration = self
            .prior
            .checked_add(u64::from(self.duration))
            .and_then(|duration| duration.checked_mul(1000))
            .ok_or(MediaError::Capacity)?
            / u64::from(rate);
        Ok(DtmfTone {
            digit: self.digit,
            duration_ms: duration.try_into().map_err(|_| MediaError::Capacity)?,
        })
    }

    fn contains(self, packet: &RtpPacket, digit: DtmfDigit) -> bool {
        let offset = packet.timestamp().wrapping_sub(self.start);
        self.ssrc == packet.ssrc()
            && self.digit == digit
            && u64::from(offset) <= self.prior
            && offset.is_multiple_of(u32::from(u16::MAX))
    }
}

pub struct TelephoneEvents {
    previous: Option<DtmfTone>,
    current: Option<DtmfTone>,
}

impl TelephoneEvents {
    pub fn into_tones(self) -> impl Iterator<Item = DtmfTone> {
        [self.previous, self.current].into_iter().flatten()
    }
}

pub struct TelephoneReceiver {
    payload_type: u8,
    rate: u32,
    source: Option<u32>,
    active: Option<Reception>,
    completed: [Option<Reception>; 16],
    cursor: usize,
}

impl TelephoneReceiver {
    pub fn new(payload_type: u8, rate: u32) -> Result<Self, MediaError> {
        if payload_type > 127 || !matches!(rate, 8000 | 48000) {
            return Err(MediaError::Unsupported);
        }
        Ok(Self {
            payload_type,
            rate,
            source: None,
            active: None,
            completed: [None; 16],
            cursor: 0,
        })
    }

    fn remember(&mut self, event: Reception) {
        self.completed[self.cursor] = Some(event);
        self.cursor = (self.cursor + 1) % self.completed.len();
    }

    pub fn expire(&mut self, now: u64) -> Result<Option<DtmfTone>, MediaError> {
        let Some(event) = self
            .active
            .filter(|event| now.saturating_sub(event.updated) >= 150)
        else {
            return Ok(None);
        };
        let tone = event.tone(self.rate)?;
        self.remember(event);
        self.active = None;
        Ok(Some(tone))
    }

    pub fn push(&mut self, packet: &RtpPacket, now: u64) -> Result<TelephoneEvents, MediaError> {
        if packet.payload_type() != self.payload_type
            || self.source.is_some_and(|source| source != packet.ssrc())
        {
            return Err(MediaError::InvalidPacket);
        }
        let payload = TelephoneEvent::parse(packet.payload())?;
        let mut output = TelephoneEvents {
            previous: None,
            current: None,
        };
        if self
            .completed
            .iter()
            .flatten()
            .any(|event| event.contains(packet, payload.digit()))
        {
            return Ok(output);
        }
        let mut current = Reception {
            ssrc: packet.ssrc(),
            digit: payload.digit(),
            start: packet.timestamp(),
            segment: packet.timestamp(),
            prior: 0,
            duration: payload.duration(),
            sequence: packet.sequence(),
            updated: now,
        };
        if let Some(mut active) = self.active {
            let distance = packet.sequence().wrapping_sub(active.sequence);
            if distance == 0 || distance >= 32768 {
                return Ok(output);
            }
            let segment_distance = packet.timestamp().wrapping_sub(active.segment);
            if segment_distance >= 1 << 31 {
                return Ok(output);
            }
            if active.digit == payload.digit() && segment_distance == 0 {
                if payload.duration() < active.duration {
                    return Ok(output);
                }
                active.duration = payload.duration();
                active.sequence = packet.sequence();
                active.updated = now;
                current = active;
            } else if active.digit == payload.digit()
                && !packet.marker()
                && segment_distance.is_multiple_of(u32::from(u16::MAX))
            {
                active.prior = active
                    .prior
                    .checked_add(u64::from(segment_distance))
                    .ok_or(MediaError::Capacity)?;
                active.segment = packet.timestamp();
                active.duration = payload.duration();
                active.sequence = packet.sequence();
                active.updated = now;
                current = active;
            } else {
                output.previous = Some(active.tone(self.rate)?);
                self.remember(active);
            }
        }
        self.source = Some(packet.ssrc());
        if payload.ended() {
            output.current = Some(current.tone(self.rate)?);
            self.remember(current);
            self.active = None;
        } else {
            self.active = Some(current);
        }
        Ok(output)
    }
}
