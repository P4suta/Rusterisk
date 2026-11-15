use bytes::Bytes;
use webrtc_util::marshal::{Marshal, Unmarshal};
mod audiosocket_stream;
mod mixing;
pub use audiosocket_stream::AudioSocketDecoder;
mod rtcp;
mod streams;
mod telephone_events;
pub use mixing::{AudioTarget, Gate, MixedAudio, Mixer};
pub use rtcp::{goodbye_reports, sender_reports, SourceReport};
pub use streams::{AudioFormat, RtpReceiver, RtpSender};
pub use telephone_events::{
    TelephoneEvent, TelephoneEvents, TelephoneFormat, TelephoneReceiver, TelephoneSender,
};

pub const MAX_SAMPLES: usize = 960;
pub const MAX_PACKET: usize = 2048;
const MAX_DECODED_SAMPLES: usize = 5760;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MediaError {
    Capacity,
    InvalidFrame,
    InvalidPacket,
    InvalidRoute,
    UnknownStream,
    Unsupported,
    Codec(String),
    Transport(String),
    Control(String),
}

impl std::fmt::Display for MediaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Capacity => f.write_str("media capacity exceeded"),
            Self::InvalidFrame => f.write_str("invalid audio frame"),
            Self::InvalidPacket => f.write_str("invalid media packet"),
            Self::InvalidRoute => f.write_str("invalid media route"),
            Self::UnknownStream => f.write_str("unknown media stream"),
            Self::Unsupported => f.write_str("unsupported media format"),
            Self::Codec(error) => write!(f, "codec: {error}"),
            Self::Transport(error) => write!(f, "transport: {error}"),
            Self::Control(error) => write!(f, "RTCP: {error}"),
        }
    }
}

impl std::error::Error for MediaError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Frame {
    samples: [i16; MAX_SAMPLES],
    len: usize,
    rate: u32,
}

impl Frame {
    pub fn new(samples: &[i16], rate: u32) -> Result<Self, MediaError> {
        if samples.is_empty()
            || samples.len() > MAX_SAMPLES
            || !matches!(rate, 8000 | 12000 | 16000 | 24000 | 32000 | 44100 | 48000)
        {
            return Err(MediaError::InvalidFrame);
        }
        let mut frame = Self {
            samples: [0; MAX_SAMPLES],
            len: samples.len(),
            rate,
        };
        frame.samples[..samples.len()].copy_from_slice(samples);
        Ok(frame)
    }

    pub fn samples(&self) -> &[i16] {
        &self.samples[..self.len]
    }
    pub const fn rate(&self) -> u32 {
        self.rate
    }

    pub fn resample(&self, rate: u32) -> Result<Self, MediaError> {
        if rate == self.rate {
            return Ok(*self);
        }
        let len = (self.len as u64 * u64::from(rate) / u64::from(self.rate)) as usize;
        if len == 0 || len > MAX_SAMPLES {
            return Err(MediaError::InvalidFrame);
        }
        let mut output = [0i16; MAX_SAMPLES];
        for (index, sample) in output.iter_mut().take(len).enumerate() {
            let position = index as u64 * u64::from(self.rate);
            let first = (position / u64::from(rate)) as usize;
            let fraction = (position % u64::from(rate)) as i64;
            let a = i64::from(self.samples[first.min(self.len - 1)]);
            let b = i64::from(self.samples[(first + 1).min(self.len - 1)]);
            *sample = (a + (b - a) * fraction / i64::from(rate)) as i16;
        }
        Self::new(&output[..len], rate)
    }

    pub fn mix(&self, other: &Self) -> Result<Self, MediaError> {
        if self.rate != other.rate || self.len != other.len {
            return Err(MediaError::InvalidFrame);
        }
        let mut output = *self;
        for (sample, other) in output.samples[..self.len].iter_mut().zip(other.samples()) {
            *sample = sample.saturating_add(*other);
        }
        Ok(output)
    }
}

pub fn mulaw_decode(value: u8) -> i16 {
    let value = !value;
    let magnitude = (((i32::from(value & 0x0f) << 3) + 0x84) << ((value & 0x70) >> 4)) - 0x84;
    if value & 0x80 == 0 {
        magnitude as i16
    } else {
        -magnitude as i16
    }
}

pub fn mulaw_encode(sample: i16) -> u8 {
    let (magnitude, mask) = if sample < 0 {
        (0x84 - i32::from(sample), 0x7f)
    } else {
        (i32::from(sample) + 0x84, 0xff)
    };
    let magnitude = magnitude.min(32767);
    let segment = [0xff, 0x1ff, 0x3ff, 0x7ff, 0xfff, 0x1fff, 0x3fff, 0x7fff]
        .iter()
        .position(|end| magnitude <= *end)
        .unwrap_or(7);
    ((segment as u8) << 4 | ((magnitude >> (segment + 3)) as u8 & 0x0f)) ^ mask
}

pub fn alaw_decode(value: u8) -> i16 {
    let value = value ^ 0x55;
    let mut magnitude = i32::from(value & 0x0f) << 4;
    let segment = (value & 0x70) >> 4;
    match segment {
        0 => magnitude += 8,
        1 => magnitude += 0x108,
        _ => {
            magnitude += 0x108;
            magnitude <<= segment - 1;
        }
    }
    if value & 0x80 == 0 {
        -magnitude as i16
    } else {
        magnitude as i16
    }
}

pub fn alaw_encode(sample: i16) -> u8 {
    let sample = i32::from(sample) >> 3;
    let (magnitude, mask) = if sample >= 0 {
        (sample, 0xd5)
    } else {
        (-sample - 1, 0x55)
    };
    let segment = [0x1f, 0x3f, 0x7f, 0xff, 0x1ff, 0x3ff, 0x7ff, 0xfff]
        .iter()
        .position(|end| magnitude <= *end)
        .unwrap_or(7);
    let quantized = if segment < 2 {
        magnitude >> 1
    } else {
        magnitude >> segment
    };
    ((segment as u8) << 4 | (quantized as u8 & 0x0f)) ^ mask
}

pub struct OpusCodec {
    encoder: opus::Encoder,
    decoder: opus::Decoder,
}

impl OpusCodec {
    pub fn new() -> Result<Self, MediaError> {
        Ok(Self {
            encoder: opus::Encoder::new(48000, opus::Channels::Mono, opus::Application::Voip)
                .map_err(|error| MediaError::Codec(error.to_string()))?,
            decoder: opus::Decoder::new(48000, opus::Channels::Mono)
                .map_err(|error| MediaError::Codec(error.to_string()))?,
        })
    }

    pub fn encode(&mut self, frame: Frame) -> Result<Vec<u8>, MediaError> {
        let frame = frame.resample(48000)?;
        let mut output = [0u8; 1275];
        let count = self
            .encoder
            .encode(frame.samples(), &mut output)
            .map_err(|error| MediaError::Codec(error.to_string()))?;
        Ok(output[..count].to_vec())
    }

    pub fn decode(&mut self, bytes: &[u8]) -> Result<Frame, MediaError> {
        let mut output = [0i16; MAX_SAMPLES];
        let count = self.decode_into(bytes, &mut output)?;
        Frame::new(&output[..count], 48000)
    }

    fn decode_into(&mut self, bytes: &[u8], output: &mut [i16]) -> Result<usize, MediaError> {
        if bytes.is_empty() || bytes.len() > MAX_PACKET - 12 || output.len() > MAX_DECODED_SAMPLES {
            return Err(MediaError::Capacity);
        }
        let expected = self
            .decoder
            .get_nb_samples(bytes)
            .map_err(|error| MediaError::Codec(error.to_string()))?;
        if expected == 0 || expected > output.len() {
            return Err(MediaError::Capacity);
        }
        let count = self
            .decoder
            .decode(bytes, output, false)
            .map_err(|error| MediaError::Codec(error.to_string()))?;
        if count != expected {
            return Err(MediaError::InvalidFrame);
        }
        Ok(count)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RtpPacket(rtp::packet::Packet);

impl RtpPacket {
    pub fn new(
        payload_type: u8,
        sequence: u16,
        timestamp: u32,
        ssrc: u32,
        payload: &[u8],
    ) -> Result<Self, MediaError> {
        if payload.len() > MAX_PACKET - 12 || payload_type > 127 {
            return Err(MediaError::Capacity);
        }
        Ok(Self(rtp::packet::Packet {
            header: rtp::header::Header {
                version: 2,
                payload_type,
                sequence_number: sequence,
                timestamp,
                ssrc,
                ..Default::default()
            },
            payload: Bytes::copy_from_slice(payload),
        }))
    }

    pub fn parse(bytes: &[u8]) -> Result<Self, MediaError> {
        if bytes.len() > MAX_PACKET || bytes.len() < 12 {
            return Err(MediaError::InvalidPacket);
        }
        if bytes[0] & 0x20 != 0 && bytes.last() == Some(&0) {
            return Err(MediaError::InvalidPacket);
        }
        let packet = rtp::packet::Packet::unmarshal(&mut &bytes[..])
            .map_err(|error| MediaError::Transport(error.to_string()))?;
        if packet.header.version != 2 || packet.payload.is_empty() {
            return Err(MediaError::InvalidPacket);
        }
        Ok(Self(packet))
    }

    pub fn encode(&self) -> Result<Bytes, MediaError> {
        self.0
            .marshal()
            .map_err(|error| MediaError::Transport(error.to_string()))
    }
    pub fn payload(&self) -> &[u8] {
        &self.0.payload
    }
    pub fn with_marker(mut self, marker: bool) -> Self {
        self.0.header.marker = marker;
        self
    }

    pub fn marker(&self) -> bool {
        self.0.header.marker
    }
    pub const fn sequence(&self) -> u16 {
        self.0.header.sequence_number
    }
    pub const fn timestamp(&self) -> u32 {
        self.0.header.timestamp
    }
    pub const fn ssrc(&self) -> u32 {
        self.0.header.ssrc
    }
    pub const fn payload_type(&self) -> u8 {
        self.0.header.payload_type
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum QueueState {
    Playing,
    Paused,
}

pub struct MediaQueue {
    frames: [Option<Frame>; 50],
    head: usize,
    len: usize,
    state: QueueState,
}

impl Default for MediaQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl MediaQueue {
    pub const fn new() -> Self {
        Self {
            frames: [None; 50],
            head: 0,
            len: 0,
            state: QueueState::Playing,
        }
    }
    pub fn push(&mut self, frame: Frame) -> Result<(), MediaError> {
        if self.len == self.frames.len() {
            return Err(MediaError::Capacity);
        }
        self.frames[(self.head + self.len) % self.frames.len()] = Some(frame);
        self.len += 1;
        Ok(())
    }
    pub fn pop(&mut self) -> Option<Frame> {
        if self.state == QueueState::Paused || self.len == 0 {
            return None;
        }
        let frame = self.frames[self.head].take();
        self.head = (self.head + 1) % self.frames.len();
        self.len -= 1;
        frame
    }
    pub fn pause(&mut self) {
        self.state = QueueState::Paused;
    }
    pub fn resume(&mut self) {
        self.state = QueueState::Playing;
    }
    pub fn flush(&mut self) {
        self.frames.fill(None);
        self.head = 0;
        self.len = 0;
    }
    pub const fn len(&self) -> usize {
        self.len
    }
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[expect(
    clippy::large_enum_variant,
    reason = "docs/adr/serialize-call-decisions-in-a-pure-core.md: Keep bounded frames inline."
)]
pub enum AudioSocket {
    Terminate,
    Uuid([u8; 16]),
    Dtmf(u8),
    Audio(Frame),
}

impl AudioSocket {
    pub fn parse(bytes: &[u8]) -> Result<Self, MediaError> {
        let [kind, high, low, payload @ ..] = bytes else {
            return Err(MediaError::InvalidPacket);
        };
        if payload.len() != usize::from(u16::from_be_bytes([*high, *low]))
            || payload.len() > MAX_SAMPLES * 2
        {
            return Err(MediaError::InvalidPacket);
        }
        match kind {
            0 if payload.is_empty() => Ok(Self::Terminate),
            1 => Ok(Self::Uuid(
                payload.try_into().map_err(|_| MediaError::InvalidPacket)?,
            )),
            3 if payload.len() == 1 && b"0123456789*#ABCD".contains(&payload[0]) => {
                Ok(Self::Dtmf(payload[0]))
            }
            0x10..=0x16 => {
                let rate = match kind {
                    0x10 => 8000,
                    0x11 => 12000,
                    0x12 => 16000,
                    0x13 => 24000,
                    0x14 => 32000,
                    0x15 => 44100,
                    0x16 => 48000,
                    _ => return Err(MediaError::Unsupported),
                };
                if !payload.len().is_multiple_of(2) || payload.is_empty() {
                    return Err(MediaError::InvalidPacket);
                }
                let mut samples = [0i16; MAX_SAMPLES];
                for (sample, bytes) in samples.iter_mut().zip(payload.as_chunks::<2>().0) {
                    *sample = i16::from_le_bytes([bytes[0], bytes[1]]);
                }
                Ok(Self::Audio(Frame::new(
                    &samples[..payload.len() / 2],
                    rate,
                )?))
            }
            _ => Err(MediaError::Unsupported),
        }
    }

    pub fn encode(&self) -> Result<Vec<u8>, MediaError> {
        let (kind, payload) = match self {
            Self::Terminate => (0, Vec::new()),
            Self::Uuid(uuid) => (1, uuid.to_vec()),
            Self::Dtmf(digit) => (3, [*digit].into_iter().collect()),
            Self::Audio(frame) => {
                let kind = match frame.rate {
                    8000 => 0x10,
                    12000 => 0x11,
                    16000 => 0x12,
                    24000 => 0x13,
                    32000 => 0x14,
                    44100 => 0x15,
                    48000 => 0x16,
                    _ => return Err(MediaError::Unsupported),
                };
                (
                    kind,
                    frame
                        .samples()
                        .iter()
                        .flat_map(|sample| sample.to_le_bytes())
                        .collect(),
                )
            }
        };
        let size = u16::try_from(payload.len()).map_err(|_| MediaError::Capacity)?;
        let mut output = Vec::with_capacity(3 + payload.len());
        output.push(kind);
        output.extend_from_slice(&size.to_be_bytes());
        output.extend_from_slice(&payload);
        Ok(output)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlKind {
    Sender,
    Receiver,
    Description,
    Goodbye,
    Application,
    TransportFeedback,
    PayloadFeedback,
    Extended,
}

pub fn inspect_rtcp(bytes: &[u8]) -> Result<Vec<ControlKind>, MediaError> {
    if bytes.len() > MAX_PACKET {
        return Err(MediaError::Capacity);
    }
    let packets = rtcp_types::Compound::parse(bytes)
        .map_err(|error| MediaError::Control(error.to_string()))?;
    let mut kinds = Vec::new();
    for packet in packets {
        let kind = match packet.map_err(|error| MediaError::Control(error.to_string()))? {
            rtcp_types::Packet::Sr(_) => ControlKind::Sender,
            rtcp_types::Packet::Rr(_) => ControlKind::Receiver,
            rtcp_types::Packet::Sdes(_) => ControlKind::Description,
            rtcp_types::Packet::Bye(_) => ControlKind::Goodbye,
            rtcp_types::Packet::App(_) => ControlKind::Application,
            rtcp_types::Packet::TransportFeedback(_) => ControlKind::TransportFeedback,
            rtcp_types::Packet::PayloadFeedback(_) => ControlKind::PayloadFeedback,
            rtcp_types::Packet::Xr(_) => ControlKind::Extended,
            rtcp_types::Packet::Unknown(_) => return Err(MediaError::Unsupported),
        };
        kinds.push(kind);
    }
    if kinds.is_empty() {
        return Err(MediaError::InvalidPacket);
    }
    Ok(kinds)
}

pub fn sender_report(
    ssrc: u32,
    ntp: u64,
    timestamp: u32,
    packets: u32,
    octets: u32,
    cname: &str,
) -> Result<Vec<u8>, MediaError> {
    sender_reports(
        &[SourceReport {
            ssrc,
            timestamp,
            packets,
            octets,
        }],
        ntp,
        cname,
    )
}

pub struct Srtp {
    key: [u8; 16],
    salt: [u8; 14],
    streams: [Option<SecureStream>; 4],
}

struct SecureStream {
    ssrc: u32,
    context: webrtc_srtp::context::Context,
}

#[derive(Clone, Copy)]
enum CipherOperation {
    Encrypt,
    Decrypt,
}

impl Srtp {
    pub fn new(key: &[u8; 16], salt: &[u8; 14]) -> Result<Self, MediaError> {
        Ok(Self {
            key: *key,
            salt: *salt,
            streams: std::array::from_fn(|_| None),
        })
    }

    fn context(&self) -> Result<webrtc_srtp::context::Context, MediaError> {
        webrtc_srtp::context::Context::new(
            &self.key,
            &self.salt,
            webrtc_srtp::protection_profile::ProtectionProfile::Aes128CmHmacSha1_80,
            Some(webrtc_srtp::option::srtp_replay_protection(128)),
            Some(webrtc_srtp::option::srtcp_replay_protection(128)),
        )
        .map_err(|error| MediaError::Transport(error.to_string()))
    }

    fn process(&mut self, packet: &[u8], operation: CipherOperation) -> Result<Bytes, MediaError> {
        let header = rtp::header::Header::unmarshal(&mut &packet[..])
            .map_err(|error| MediaError::Transport(error.to_string()))?;
        if header.version != 2 {
            return Err(MediaError::InvalidPacket);
        }
        fn transform(
            context: &mut webrtc_srtp::context::Context,
            packet: &[u8],
            operation: CipherOperation,
        ) -> Result<Bytes, MediaError> {
            match operation {
                CipherOperation::Encrypt => context.encrypt_rtp(packet),
                CipherOperation::Decrypt => context.decrypt_rtp(packet),
            }
            .map_err(|error| MediaError::Transport(error.to_string()))
        }
        for stream in self.streams.iter_mut().flatten() {
            if stream.ssrc == header.ssrc {
                return transform(&mut stream.context, packet, operation);
            }
        }
        let slot = self
            .streams
            .iter()
            .position(Option::is_none)
            .ok_or(MediaError::Capacity)?;
        let mut context = self.context()?;
        let output = transform(&mut context, packet, operation)?;
        self.streams[slot] = Some(SecureStream {
            ssrc: header.ssrc,
            context,
        });
        Ok(output)
    }
    pub fn encrypt(&mut self, packet: &[u8]) -> Result<Bytes, MediaError> {
        if packet.len() > MAX_PACKET {
            return Err(MediaError::Capacity);
        }
        self.process(packet, CipherOperation::Encrypt)
    }
    pub fn decrypt(&mut self, packet: &[u8]) -> Result<Bytes, MediaError> {
        if packet.len() > MAX_PACKET + 16 {
            return Err(MediaError::Capacity);
        }
        self.process(packet, CipherOperation::Decrypt)
    }
}
