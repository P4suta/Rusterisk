use crate::{
    alaw_decode, alaw_encode, mulaw_decode, mulaw_encode, Frame, MediaError, OpusCodec, PcmFramer,
    RtpPacket, MAX_DECODED_SAMPLES, MAX_PACKET, MAX_SAMPLES,
};
use std::collections::VecDeque;

struct DecodedAudio<'a> {
    samples: &'a [i16],
    rate: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioFormat {
    Pcmu,
    Pcma,
    Opus,
}

impl AudioFormat {
    pub const fn rate(self) -> u32 {
        match self {
            Self::Pcmu | Self::Pcma => 8000,
            Self::Opus => 48000,
        }
    }
}

enum Codec {
    Pcmu,
    Pcma,
    Opus(OpusCodec),
}

impl Codec {
    fn new(format: AudioFormat) -> Result<Self, MediaError> {
        match format {
            AudioFormat::Pcmu => Ok(Self::Pcmu),
            AudioFormat::Pcma => Ok(Self::Pcma),
            AudioFormat::Opus => Ok(Self::Opus(OpusCodec::new()?)),
        }
    }

    fn encode(&mut self, frame: Frame) -> Result<Vec<u8>, MediaError> {
        match self {
            Self::Pcmu => Ok(frame
                .resample(8000)?
                .samples()
                .iter()
                .map(|sample| mulaw_encode(*sample))
                .collect()),
            Self::Pcma => Ok(frame
                .resample(8000)?
                .samples()
                .iter()
                .map(|sample| alaw_encode(*sample))
                .collect()),
            Self::Opus(codec) => codec.encode(frame),
        }
    }

    fn decoded_samples(&self, bytes: &[u8]) -> Result<usize, MediaError> {
        if bytes.is_empty() || bytes.len() > MAX_PACKET - 12 {
            return Err(MediaError::InvalidPacket);
        }
        let count = match self {
            Self::Pcmu | Self::Pcma => {
                if bytes.len() > 1600 {
                    return Err(MediaError::Capacity);
                }
                bytes.len()
            }
            Self::Opus(codec) => codec
                .decoder
                .get_nb_samples(bytes)
                .map_err(|error| MediaError::Codec(error.to_string()))?,
        };
        if count == 0 || count > MAX_DECODED_SAMPLES {
            return Err(MediaError::Capacity);
        }
        Ok(count)
    }

    fn decode<'a>(
        &mut self,
        bytes: &[u8],
        samples: &'a mut [i16; MAX_DECODED_SAMPLES],
    ) -> Result<DecodedAudio<'a>, MediaError> {
        if bytes.is_empty() || bytes.len() > MAX_PACKET - 12 {
            return Err(MediaError::InvalidPacket);
        }
        match self {
            Self::Pcmu | Self::Pcma => {
                if bytes.len() > 1600 {
                    return Err(MediaError::Capacity);
                }
                for (sample, byte) in samples.iter_mut().zip(bytes) {
                    *sample = match self {
                        Self::Pcmu => mulaw_decode(*byte),
                        Self::Pcma => alaw_decode(*byte),
                        Self::Opus(_) => return Err(MediaError::Unsupported),
                    };
                }
                Ok(DecodedAudio {
                    samples: &samples[..bytes.len()],
                    rate: 8000,
                })
            }
            Self::Opus(codec) => {
                let count = codec.decode_into(bytes, samples)?;
                Ok(DecodedAudio {
                    samples: &samples[..count],
                    rate: 48000,
                })
            }
        }
    }
}

pub struct AudioDecoder {
    codec: Codec,
    frames: PcmFramer,
}

impl AudioDecoder {
    pub fn new(format: AudioFormat) -> Result<Self, MediaError> {
        Ok(Self {
            codec: Codec::new(format)?,
            frames: PcmFramer::new(format.rate())?,
        })
    }

    pub fn push(&mut self, bytes: &[u8]) -> Result<(), MediaError> {
        let count = self.codec.decoded_samples(bytes)?;
        if !self.frames.can_push(count) {
            return Err(MediaError::Capacity);
        }
        let mut samples = [0; MAX_DECODED_SAMPLES];
        let decoded = self.codec.decode(bytes, &mut samples)?;
        for chunk in decoded.samples.chunks(MAX_SAMPLES) {
            self.frames.push(Frame::new(chunk, decoded.rate)?)?;
        }
        Ok(())
    }

    pub fn pop(&mut self) -> Option<Frame> {
        self.frames.pop()
    }

    pub fn clear(&mut self) {
        self.frames.clear();
    }

    pub fn reset(&mut self, format: AudioFormat) -> Result<(), MediaError> {
        *self = Self::new(format)?;
        Ok(())
    }
}

pub struct RtpSender {
    format: AudioFormat,
    codec: Codec,
    payload_type: u8,
    pub(crate) clock: PacketClock,
    timestamp_base: u32,
    first: bool,
}

pub(crate) struct PacketClock {
    pub(crate) ssrc: u32,
    sequence: u16,
    pub(crate) timestamp: u32,
}

impl PacketClock {
    pub(crate) fn new(seed: [u8; 10]) -> Self {
        Self {
            ssrc: u32::from_be_bytes([seed[0], seed[1], seed[2], seed[3]]),
            sequence: u16::from_be_bytes([seed[4], seed[5]]),
            timestamp: u32::from_be_bytes([seed[6], seed[7], seed[8], seed[9]]),
        }
    }

    pub(crate) fn packet(
        &mut self,
        payload_type: u8,
        timestamp: u32,
        marker: bool,
        payload: &[u8],
    ) -> Result<RtpPacket, MediaError> {
        let packet = RtpPacket::new(payload_type, self.sequence, timestamp, self.ssrc, payload)?
            .with_marker(marker);
        self.sequence = self.sequence.wrapping_add(1);
        Ok(packet)
    }
}

impl RtpSender {
    pub fn new(format: AudioFormat, payload_type: u8, seed: [u8; 10]) -> Result<Self, MediaError> {
        if payload_type > 127 {
            return Err(MediaError::InvalidPacket);
        }
        Ok(Self {
            format,
            codec: Codec::new(format)?,
            payload_type,
            clock: PacketClock::new(seed),
            timestamp_base: u32::from_be_bytes([seed[6], seed[7], seed[8], seed[9]]),
            first: true,
        })
    }

    pub fn packet(&mut self, frame: Frame) -> Result<RtpPacket, MediaError> {
        self.encode_packet(frame, self.clock.timestamp)
    }

    pub fn packet_at(&mut self, frame: Frame, now_ms: u64) -> Result<RtpPacket, MediaError> {
        self.encode_packet(frame, self.timestamp_at(now_ms))
    }

    pub const fn timestamp_at(&self, now_ms: u64) -> u32 {
        self.timestamp_base
            .wrapping_add(now_ms.wrapping_mul(self.format.rate() as u64 / 1000) as u32)
    }

    fn encode_packet(&mut self, frame: Frame, timestamp: u32) -> Result<RtpPacket, MediaError> {
        let samples = frame.resample(self.format.rate())?;
        let payload = self.codec.encode(samples)?;
        let packet = self
            .clock
            .packet(self.payload_type, timestamp, self.first, &payload)?;
        self.first = false;
        self.clock.timestamp = timestamp.wrapping_add(samples.samples().len() as u32);
        Ok(packet)
    }

    pub(crate) const fn rate(&self) -> u32 {
        self.format.rate()
    }
}

enum Received {
    Audio(RtpPacket),
    Signal(u16),
}

struct FrameAssembly {
    partial: [i16; MAX_SAMPLES],
    filled: usize,
    rate: u32,
    timestamp: Option<u32>,
    ready: VecDeque<Frame>,
}

impl FrameAssembly {
    fn new(format: AudioFormat) -> Self {
        Self {
            partial: [0; MAX_SAMPLES],
            filled: 0,
            rate: format.rate(),
            timestamp: None,
            ready: VecDeque::with_capacity(10),
        }
    }

    fn clear(&mut self) {
        self.filled = 0;
        self.timestamp = None;
        self.ready.clear();
    }

    fn push(&mut self, audio: DecodedAudio<'_>, timestamp: u32) -> Result<(), MediaError> {
        if audio.rate != self.rate {
            return Err(MediaError::InvalidFrame);
        }
        let frame_len = self.rate as usize / 50;
        let filled = if self.timestamp == Some(timestamp) {
            self.filled
        } else {
            0
        };
        if self.ready.len() + (filled + audio.samples.len()) / frame_len > 10 {
            return Err(MediaError::Capacity);
        }
        self.filled = filled;
        for sample in audio.samples {
            self.partial[self.filled] = *sample;
            self.filled += 1;
            if self.filled == frame_len {
                self.ready
                    .push_back(Frame::new(&self.partial[..frame_len], self.rate)?);
                self.filled = 0;
            }
        }
        self.timestamp = Some(timestamp.wrapping_add(audio.samples.len() as u32));
        Ok(())
    }
}

impl Received {
    fn sequence(&self) -> u16 {
        match self {
            Self::Audio(packet) => packet.sequence(),
            Self::Signal(sequence) => *sequence,
        }
    }
}

pub struct RtpReceiver {
    codec: Codec,
    payload_type: u8,
    ssrc: Option<u32>,
    next: Option<u16>,
    packets: Vec<Received>,
    assembly: FrameAssembly,
}

impl RtpReceiver {
    pub fn discard_pending(&mut self) {
        self.packets.clear();
        self.next = None;
        self.assembly.clear();
    }

    pub fn new(format: AudioFormat, payload_type: u8) -> Result<Self, MediaError> {
        if payload_type > 127 {
            return Err(MediaError::InvalidPacket);
        }
        Ok(Self {
            codec: Codec::new(format)?,
            payload_type,
            ssrc: None,
            next: None,
            packets: Vec::with_capacity(16),
            assembly: FrameAssembly::new(format),
        })
    }

    pub fn push(&mut self, packet: &RtpPacket) -> Result<bool, MediaError> {
        if packet.payload_type() != self.payload_type
            || packet.payload().is_empty()
            || self.ssrc.is_some_and(|ssrc| ssrc != packet.ssrc())
        {
            return Err(MediaError::InvalidPacket);
        }
        let accepted = self.insert(Received::Audio(packet.clone()))?;
        if accepted {
            self.ssrc = Some(packet.ssrc());
        }
        Ok(accepted)
    }

    pub fn skip(&mut self, packet: &RtpPacket) -> Result<bool, MediaError> {
        if self.ssrc != Some(packet.ssrc()) {
            return Ok(false);
        }
        self.insert(Received::Signal(packet.sequence()))
    }

    fn insert(&mut self, packet: Received) -> Result<bool, MediaError> {
        let mut next = self.next.unwrap_or(packet.sequence());
        let mut distance = packet.sequence().wrapping_sub(next);
        if distance >= 32768
            || self
                .packets
                .iter()
                .any(|stored| stored.sequence() == packet.sequence())
        {
            return Ok(false);
        }
        if distance >= 16 && self.packets.is_empty() {
            next = packet.sequence();
            distance = 0;
        }
        if distance >= 16 || self.packets.len() >= 16 {
            return Err(MediaError::Capacity);
        }
        self.next = Some(next);
        self.packets.push(packet);
        Ok(true)
    }

    pub fn pop(&mut self) -> Result<Option<Frame>, MediaError> {
        if let Some(frame) = self.assembly.ready.pop_front() {
            return Ok(Some(frame));
        }
        while !self.packets.is_empty() {
            let Some(next) = self.next else {
                return Ok(None);
            };
            self.next = Some(next.wrapping_add(1));
            let Some(index) = self
                .packets
                .iter()
                .position(|packet| packet.sequence() == next)
            else {
                return Ok(None);
            };
            match self.packets.remove(index) {
                Received::Audio(packet) => {
                    let mut samples = [0; MAX_DECODED_SAMPLES];
                    let decoded = match self.codec.decode(packet.payload(), &mut samples) {
                        Ok(decoded) => decoded,
                        Err(error) => {
                            self.assembly.clear();
                            return Err(error);
                        }
                    };
                    self.assembly.push(decoded, packet.timestamp())?;
                    if let Some(frame) = self.assembly.ready.pop_front() {
                        return Ok(Some(frame));
                    }
                }
                Received::Signal(_) => {}
            }
        }
        Ok(None)
    }
}
