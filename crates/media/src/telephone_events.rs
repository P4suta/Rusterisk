use crate::streams::PacketClock;
use crate::{MediaError, RtpPacket, RtpSender};
use rusterisk_core::{DtmfDigit, DtmfTone};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TelephoneFormat {
    payload_type: u8,
    rate: u32,
    events: u16,
}

impl TelephoneFormat {
    pub fn new(payload_type: u8, rate: u32) -> Result<Self, MediaError> {
        if payload_type > 127 || !matches!(rate, 8000 | 48000) {
            return Err(MediaError::Unsupported);
        }
        Ok(Self {
            payload_type,
            rate,
            events: u16::MAX,
        })
    }

    pub const fn payload_type(self) -> u8 {
        self.payload_type
    }
    pub const fn rate(self) -> u32 {
        self.rate
    }

    pub fn with_events(mut self, events: u16) -> Result<Self, MediaError> {
        if events == 0 {
            return Err(MediaError::Unsupported);
        }
        self.events = events;
        Ok(self)
    }

    pub const fn supports(self, digit: DtmfDigit) -> bool {
        self.events & (1 << digit.event()) != 0
    }

    pub const fn events(self) -> u16 {
        self.events
    }
}

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
        TelephoneFormat::new(payload_type, rate)?;
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

enum EventSource {
    Shared,
    Separate(PacketClock),
}

struct Transmission {
    tone: DtmfTone,
    started: u64,
    timestamp: u32,
    segment: u32,
    next: u64,
    first: bool,
}

struct Ending {
    payload: [u8; 4],
    timestamp: u32,
    remaining: u8,
    next: u64,
}

pub struct TelephoneSender {
    payload_type: u8,
    rate: u32,
    events: u16,
    source: EventSource,
    active: Option<Transmission>,
    endings: Vec<Ending>,
}

impl TelephoneSender {
    pub fn new(
        audio: &RtpSender,
        payload_type: u8,
        rate: u32,
        seed: [u8; 10],
    ) -> Result<Self, MediaError> {
        Self::with_format(audio, TelephoneFormat::new(payload_type, rate)?, seed)
    }

    pub fn with_format(
        audio: &RtpSender,
        format: TelephoneFormat,
        seed: [u8; 10],
    ) -> Result<Self, MediaError> {
        let rate = format.rate;
        let source = if audio.rate() == rate {
            EventSource::Shared
        } else {
            let mut clock = PacketClock::new(seed);
            if clock.ssrc == audio.clock.ssrc {
                clock.ssrc ^= 1;
            }
            EventSource::Separate(clock)
        };
        Ok(Self {
            payload_type: format.payload_type,
            rate,
            events: format.events,
            source,
            active: None,
            endings: Vec::with_capacity(2),
        })
    }

    pub fn ready(&self) -> bool {
        self.active.is_none() && self.endings.len() < 2
    }

    pub fn cancel(&mut self) {
        self.active = None;
        self.endings.clear();
    }

    pub fn start(&mut self, tone: DtmfTone, now: u64, audio: &RtpSender) -> Result<(), MediaError> {
        if !self.ready() {
            return Err(MediaError::Capacity);
        }
        if !(1..=5000).contains(&tone.duration_ms) {
            return Err(MediaError::InvalidPacket);
        }
        if self.events & (1 << tone.digit.event()) == 0 {
            return Err(MediaError::Unsupported);
        }
        now.checked_add(u64::from(tone.duration_ms) + 40)
            .ok_or(MediaError::Capacity)?;
        let timestamp = self.timestamp_at(now, audio);
        self.active = Some(Transmission {
            tone,
            started: now,
            timestamp,
            segment: 0,
            next: now + u64::from(tone.duration_ms.min(20)),
            first: true,
        });
        Ok(())
    }

    pub fn timestamp_at(&self, now_ms: u64, audio: &RtpSender) -> u32 {
        match &self.source {
            EventSource::Shared => audio.timestamp_at(now_ms),
            EventSource::Separate(clock) => clock
                .timestamp
                .wrapping_add(now_ms.wrapping_mul(u64::from(self.rate) / 1000) as u32),
        }
    }

    fn packet(
        &mut self,
        audio: &mut RtpSender,
        timestamp: u32,
        marker: bool,
        payload: &[u8; 4],
    ) -> Result<RtpPacket, MediaError> {
        match &mut self.source {
            EventSource::Shared => &mut audio.clock,
            EventSource::Separate(clock) => clock,
        }
        .packet(self.payload_type, timestamp, marker, payload)
    }

    pub fn poll(&mut self, now: u64, audio: &mut RtpSender) -> Result<Vec<RtpPacket>, MediaError> {
        let mut packets = Vec::with_capacity(4);
        let mut index = 0;
        while index < self.endings.len() {
            if now < self.endings[index].next {
                index += 1;
                continue;
            }
            let next = now.checked_add(20).ok_or(MediaError::Capacity)?;
            let timestamp = self.endings[index].timestamp;
            let payload = self.endings[index].payload;
            packets.push(self.packet(audio, timestamp, false, &payload)?);
            self.endings[index].remaining -= 1;
            self.endings[index].next = next;
            if self.endings[index].remaining == 0 {
                self.endings.remove(index);
            } else {
                index += 1;
            }
        }
        let Some(mut active) = self.active.take() else {
            return Ok(packets);
        };
        if now < active.next {
            self.active = Some(active);
            return Ok(packets);
        }
        let next = now.checked_add(20).ok_or(MediaError::Capacity)?;
        let elapsed = now
            .saturating_sub(active.started)
            .min(u64::from(active.tone.duration_ms));
        let ticks = (elapsed * u64::from(self.rate) / 1000) as u32;
        let segment = ticks.saturating_sub(1) / u32::from(u16::MAX) * u32::from(u16::MAX);
        if segment != active.segment {
            let payload = TelephoneEvent::new(active.tone.digit, false, 10, u16::MAX)?.encode();
            packets.push(self.packet(
                audio,
                active.timestamp.wrapping_add(active.segment),
                active.first,
                &payload,
            )?);
            active.first = false;
            active.segment = segment;
        }
        let ended = elapsed == u64::from(active.tone.duration_ms);
        let payload =
            TelephoneEvent::new(active.tone.digit, ended, 10, (ticks - segment) as u16)?.encode();
        let timestamp = active.timestamp.wrapping_add(segment);
        packets.push(self.packet(audio, timestamp, active.first, &payload)?);
        if ended {
            if self.endings.len() >= 2 {
                return Err(MediaError::Capacity);
            }
            self.endings.push(Ending {
                payload,
                timestamp,
                remaining: 2,
                next,
            });
        } else {
            active.next = next;
            active.first = false;
            self.active = Some(active);
        }
        Ok(packets)
    }
}
