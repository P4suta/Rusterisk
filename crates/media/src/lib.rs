use bytes::{Bytes, BytesMut};
use rtc_crypto::providers::RingCrypto;
use rtc_shared::marshal::Unmarshal;
mod audiosocket_stream;
mod mixing;
mod pcm_framing;
pub use audiosocket_stream::AudioSocketDecoder;
pub use pcm_framing::PcmFramer;
mod rtcp;
mod streams;
mod telephone_events;
pub use mixing::{AudioTarget, Gate, MixedAudio, Mixer};
pub use rtcp::{goodbye_reports, reception_report, sender_reports, ReceptionReport, SourceReport};
pub use streams::{AudioDecoder, AudioFormat, RtpReceiver, RtpSender};
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

// The largest encoded frame, which an Opus packet bounds.
const MAX_ENCODED_AUDIO: usize = 1275;

// One encoded audio frame in a fixed buffer, which the sender borrows as bytes.
pub struct EncodedAudio {
    bytes: [u8; MAX_ENCODED_AUDIO],
    length: usize,
}

impl EncodedAudio {
    // Encodes each sample into one byte.
    pub(crate) fn bytes_of(
        samples: &[i16],
        encode: impl Fn(i16) -> u8,
    ) -> Result<Self, MediaError> {
        let mut bytes = [0; MAX_ENCODED_AUDIO];
        let slots = bytes.get_mut(..samples.len()).ok_or(MediaError::Capacity)?;
        for (slot, sample) in slots.iter_mut().zip(samples) {
            *slot = encode(*sample);
        }
        Ok(Self {
            bytes,
            length: samples.len(),
        })
    }
}

impl AsRef<[u8]> for EncodedAudio {
    fn as_ref(&self) -> &[u8] {
        &self.bytes[..self.length]
    }
}

impl std::ops::Deref for EncodedAudio {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        self.as_ref()
    }
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

    pub fn encode(&mut self, frame: Frame) -> Result<EncodedAudio, MediaError> {
        let frame = frame.resample(48000)?;
        let mut bytes = [0u8; MAX_ENCODED_AUDIO];
        let length = self
            .encoder
            .encode(frame.samples(), &mut bytes)
            .map_err(|error| MediaError::Codec(error.to_string()))?;
        Ok(EncodedAudio { bytes, length })
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

// The fixed part of a Real-time Transport Protocol header.
const RTP_HEADER: usize = 12;
const MAX_RTP_PAYLOAD: usize = MAX_PACKET - RTP_HEADER;

// A Real-time Transport Protocol packet in a fixed buffer.
// A sender builds it, and a receiver keeps the fields it reads.
#[derive(Clone)]
pub struct RtpPacket {
    payload_type: u8,
    marker: bool,
    sequence: u16,
    timestamp: u32,
    ssrc: u32,
    length: usize,
    payload: [u8; MAX_RTP_PAYLOAD],
}

impl std::fmt::Debug for RtpPacket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RtpPacket")
            .field("payload_type", &self.payload_type)
            .field("marker", &self.marker)
            .field("sequence", &self.sequence)
            .field("timestamp", &self.timestamp)
            .field("ssrc", &self.ssrc)
            .field("payload", &self.payload())
            .finish()
    }
}

impl PartialEq for RtpPacket {
    fn eq(&self, other: &Self) -> bool {
        self.payload_type == other.payload_type
            && self.marker == other.marker
            && self.sequence == other.sequence
            && self.timestamp == other.timestamp
            && self.ssrc == other.ssrc
            && self.payload() == other.payload()
    }
}

impl Eq for RtpPacket {}

// An encoded packet in a fixed buffer, which the sender borrows as bytes.
pub struct EncodedRtp {
    bytes: [u8; MAX_PACKET],
    length: usize,
}

impl AsRef<[u8]> for EncodedRtp {
    fn as_ref(&self) -> &[u8] {
        &self.bytes[..self.length]
    }
}

impl std::ops::Deref for EncodedRtp {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        self.as_ref()
    }
}

impl RtpPacket {
    pub fn new(
        payload_type: u8,
        sequence: u16,
        timestamp: u32,
        ssrc: u32,
        payload: &[u8],
    ) -> Result<Self, MediaError> {
        if payload.len() > MAX_RTP_PAYLOAD || payload_type > 127 {
            return Err(MediaError::Capacity);
        }
        let mut stored = [0; MAX_RTP_PAYLOAD];
        stored[..payload.len()].copy_from_slice(payload);
        Ok(Self {
            payload_type,
            marker: false,
            sequence,
            timestamp,
            ssrc,
            length: payload.len(),
            payload: stored,
        })
    }

    // Reads a received packet, skipping contributing sources and a header extension and dropping padding.
    pub fn parse(bytes: &[u8]) -> Result<Self, MediaError> {
        let Some([first, second, sequence_high, sequence_low, t0, t1, t2, t3, s0, s1, s2, s3, ..]) =
            bytes.get(..)
        else {
            return Err(MediaError::InvalidPacket);
        };
        if bytes.len() > MAX_PACKET || first >> 6 != 2 {
            return Err(MediaError::InvalidPacket);
        }
        let mut header_len = RTP_HEADER + usize::from(first & 0x0f) * 4;
        if first & 0x10 != 0 {
            let Some([_, _, high, low, ..]) = bytes.get(header_len..) else {
                return Err(MediaError::InvalidPacket);
            };
            header_len += 4 + 4 * usize::from(u16::from_be_bytes([*high, *low]));
        }
        let payload = bytes.get(header_len..).ok_or(MediaError::InvalidPacket)?;
        let padding = if first & 0x20 != 0 {
            let count = payload.last().copied().filter(|&count| count != 0);
            usize::from(count.ok_or(MediaError::InvalidPacket)?)
        } else {
            0
        };
        if payload.len() <= padding {
            return Err(MediaError::InvalidPacket);
        }
        let packet = Self::new(
            second & 0x7f,
            u16::from_be_bytes([*sequence_high, *sequence_low]),
            u32::from_be_bytes([*t0, *t1, *t2, *t3]),
            u32::from_be_bytes([*s0, *s1, *s2, *s3]),
            &payload[..payload.len() - padding],
        )?;
        Ok(packet.with_marker(second & 0x80 != 0))
    }

    // Writes the packet with the fixed header that a sender uses.
    pub fn encode(&self) -> Result<EncodedRtp, MediaError> {
        let mut bytes = [0; MAX_PACKET];
        let length = RTP_HEADER + self.length;
        let (header, rest) = bytes.split_at_mut(RTP_HEADER);
        header[0] = 0x80;
        header[1] = u8::from(self.marker) << 7 | self.payload_type;
        header[2..4].copy_from_slice(&self.sequence.to_be_bytes());
        header[4..8].copy_from_slice(&self.timestamp.to_be_bytes());
        header[8..12].copy_from_slice(&self.ssrc.to_be_bytes());
        rest[..self.length].copy_from_slice(self.payload());
        Ok(EncodedRtp { bytes, length })
    }
    pub fn payload(&self) -> &[u8] {
        &self.payload[..self.length]
    }
    pub fn with_marker(mut self, marker: bool) -> Self {
        self.marker = marker;
        self
    }

    pub fn marker(&self) -> bool {
        self.marker
    }
    pub const fn sequence(&self) -> u16 {
        self.sequence
    }
    pub const fn timestamp(&self) -> u32 {
        self.timestamp
    }
    pub const fn ssrc(&self) -> u32 {
        self.ssrc
    }
    pub const fn payload_type(&self) -> u8 {
        self.payload_type
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
    rtcp::inspect_rtcp(bytes)
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
    context: rtc_srtp::context::Context,
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

    fn context(&self) -> Result<rtc_srtp::context::Context, MediaError> {
        rtc_srtp::context::Context::new(
            &self.key,
            &self.salt,
            rtc_srtp::protection_profile::ProtectionProfile::Aes128CmHmacSha1_80,
            Some(rtc_srtp::option::srtp_replay_protection(128)),
            Some(rtc_srtp::option::srtcp_replay_protection(128)),
            &RingCrypto,
        )
        .map_err(|error| MediaError::Transport(error.to_string()))
    }

    fn process(&mut self, packet: &[u8], operation: CipherOperation) -> Result<Bytes, MediaError> {
        let header = rtc_rtp::header::Header::unmarshal(&mut &packet[..])
            .map_err(|error| MediaError::Transport(error.to_string()))?;
        if header.version != 2 {
            return Err(MediaError::InvalidPacket);
        }
        fn transform(
            context: &mut rtc_srtp::context::Context,
            packet: &[u8],
            operation: CipherOperation,
        ) -> Result<Bytes, MediaError> {
            match operation {
                CipherOperation::Encrypt => context.encrypt_rtp(packet),
                CipherOperation::Decrypt => context.decrypt_rtp(packet),
            }
            .map(BytesMut::freeze)
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
