use crate::{
    alaw_decode, alaw_encode, mulaw_decode, mulaw_encode, Frame, MediaError, OpusCodec, RtpPacket,
    MAX_SAMPLES,
};

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

    fn decode(&mut self, bytes: &[u8]) -> Result<Frame, MediaError> {
        if bytes.is_empty() || bytes.len() > 1275 {
            return Err(MediaError::InvalidPacket);
        }
        match self {
            Self::Pcmu | Self::Pcma => {
                if bytes.len() > MAX_SAMPLES {
                    return Err(MediaError::Capacity);
                }
                let mut samples = [0; MAX_SAMPLES];
                for (sample, byte) in samples.iter_mut().zip(bytes) {
                    *sample = match self {
                        Self::Pcmu => mulaw_decode(*byte),
                        Self::Pcma => alaw_decode(*byte),
                        Self::Opus(_) => return Err(MediaError::Unsupported),
                    };
                }
                Frame::new(&samples[..bytes.len()], 8000)
            }
            Self::Opus(codec) => codec.decode(bytes),
        }
    }
}

pub struct RtpSender {
    format: AudioFormat,
    codec: Codec,
    payload_type: u8,
    ssrc: u32,
    sequence: u16,
    timestamp: u32,
    first: bool,
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
            ssrc: u32::from_be_bytes([seed[0], seed[1], seed[2], seed[3]]),
            sequence: u16::from_be_bytes([seed[4], seed[5]]),
            timestamp: u32::from_be_bytes([seed[6], seed[7], seed[8], seed[9]]),
            first: true,
        })
    }

    pub fn packet(&mut self, frame: Frame) -> Result<RtpPacket, MediaError> {
        let samples = frame.resample(self.format.rate())?;
        let payload = self.codec.encode(samples)?;
        let packet = RtpPacket::new(
            self.payload_type,
            self.sequence,
            self.timestamp,
            self.ssrc,
            &payload,
        )?
        .with_marker(self.first);
        self.first = false;
        self.sequence = self.sequence.wrapping_add(1);
        self.timestamp = self.timestamp.wrapping_add(samples.samples().len() as u32);
        Ok(packet)
    }
}

enum Received {
    Audio(RtpPacket),
    Signal(u16),
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
}

impl RtpReceiver {
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
                Received::Audio(packet) => return self.codec.decode(packet.payload()).map(Some),
                Received::Signal(_) => {}
            }
        }
        Ok(None)
    }
}
