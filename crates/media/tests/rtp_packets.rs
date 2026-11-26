use rusterisk_media::{EncodedAudio, MediaError, RtpPacket, MAX_PACKET};

// A fixed sequence, so that every run compares the same packets.
struct Sequence(u64);

impl Sequence {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }

    fn byte(&mut self) -> u8 {
        self.next().to_le_bytes()[0]
    }

    fn bytes(&mut self, count: u64) -> Vec<u8> {
        (0..count).map(|_| self.byte()).collect()
    }
}

// The fields that a receiver reads from a packet.
#[derive(Debug, PartialEq, Eq)]
struct Fields {
    payload_type: u8,
    sequence: u16,
    timestamp: u32,
    ssrc: u32,
    marker: bool,
    payload: Vec<u8>,
}

impl Fields {
    fn of(packet: &RtpPacket) -> Self {
        Self {
            payload_type: packet.payload_type(),
            sequence: packet.sequence(),
            timestamp: packet.timestamp(),
            ssrc: packet.ssrc(),
            marker: packet.marker(),
            payload: packet.payload().to_vec(),
        }
    }
}

// The parser that the media crate used with the RTP library, kept as the reference.
fn reference(bytes: &[u8]) -> Option<Fields> {
    use rtc_rtp::header::{Header, CC_MASK, CSRC_LENGTH, CSRC_OFFSET};
    use rtc_shared::marshal::Unmarshal;
    if bytes.len() > MAX_PACKET || bytes.len() < CSRC_OFFSET || bytes[0] >> 6 != 2 {
        return None;
    }
    let mut header_len = CSRC_OFFSET + usize::from(bytes[0] & CC_MASK) * CSRC_LENGTH;
    if bytes[0] & 0x10 != 0 {
        let [_, _, high, low, ..] = bytes.get(header_len..)? else {
            return None;
        };
        header_len += 4 + 4 * usize::from(u16::from_be_bytes([*high, *low]));
    }
    let (mut header_bytes, payload) = bytes.split_at_checked(header_len)?;
    let header = Header::unmarshal(&mut header_bytes).ok()?;
    let padding = if header.padding {
        usize::from(payload.last().copied().filter(|&count| count != 0)?)
    } else {
        0
    };
    if payload.len() <= padding {
        return None;
    }
    Some(Fields {
        payload_type: header.payload_type,
        sequence: header.sequence_number,
        timestamp: header.timestamp,
        ssrc: header.ssrc,
        marker: header.marker,
        payload: payload[..payload.len() - padding].to_vec(),
    })
}

// Builds a packet with every optional header part that a peer may send, and sometimes breaks it.
fn generated(sequence: &mut Sequence) -> Vec<u8> {
    let version = if sequence.below(16) == 0 { 1 } else { 2 };
    let padding = sequence.below(4) == 0;
    let extension = sequence.below(3) == 0;
    let contributors = sequence.below(4);
    let mut bytes = Vec::from([
        version << 6 | u8::from(padding) << 5 | u8::from(extension) << 4 | contributors as u8,
        sequence.byte(),
    ]);
    bytes.extend(sequence.bytes(10));
    bytes.extend(sequence.bytes(4 * contributors));
    if extension {
        // One-byte and two-byte element profiles appear as often as other profiles.
        let profile = match sequence.below(3) {
            0 => [0xbe, 0xde],
            1 => [0x10, sequence.byte() & 0x0f],
            _ => [sequence.byte(), sequence.byte()],
        };
        let words = sequence.below(3);
        bytes.extend([profile[0], profile[1], 0, words as u8]);
        bytes.extend(sequence.bytes(4 * words));
    }
    let length = sequence.below(200);
    bytes.extend(sequence.bytes(length));
    if padding {
        let count = sequence.below(8) as u8;
        bytes.extend(std::iter::repeat_n(0, usize::from(count.saturating_sub(1))));
        bytes.push(count);
    }
    if sequence.below(10) == 0 {
        let length = sequence.below(bytes.len() as u64 + 1) as usize;
        bytes.truncate(length);
    }
    bytes
}

// Whether a value of the type of the argument owns memory that its drop releases.
const fn needs_drop_of<T>(_value: &T) -> bool {
    std::mem::needs_drop::<T>()
}

#[test]
fn a_packet_and_its_encoding_hold_no_heap_memory() -> Result<(), MediaError> {
    assert!(!std::mem::needs_drop::<RtpPacket>());
    assert!(!std::mem::needs_drop::<EncodedAudio>());
    let packet = RtpPacket::new(0, 1, 160, 7, &[0x55; 160])?;
    let encoded = packet.encode()?;
    assert!(!needs_drop_of(&encoded));
    Ok(())
}

#[test]
fn parsing_agrees_with_the_reference_over_generated_packets() {
    let mut sequence = Sequence(7);
    for round in 0..5000 {
        let bytes = generated(&mut sequence);
        let parsed = RtpPacket::parse(&bytes)
            .ok()
            .map(|packet| Fields::of(&packet));
        assert_eq!(parsed, reference(&bytes), "round {round}: {bytes:?}");
    }
}

#[test]
fn encoding_agrees_with_the_rtp_library_over_generated_packets() -> Result<(), MediaError> {
    use rtc_shared::marshal::Marshal;
    let mut sequence = Sequence(11);
    for round in 0..2000 {
        let payload_type = sequence.below(128) as u8;
        let number = sequence.next() as u16;
        let timestamp = sequence.next() as u32;
        let ssrc = sequence.next() as u32;
        let marker = sequence.below(2) == 0;
        let length = sequence.below(1200);
        let payload = sequence.bytes(length);
        let packet =
            RtpPacket::new(payload_type, number, timestamp, ssrc, &payload)?.with_marker(marker);
        let reference = rtc_rtp::packet::Packet {
            header: rtc_rtp::header::Header {
                version: 2,
                marker,
                payload_type,
                sequence_number: number,
                timestamp,
                ssrc,
                ..Default::default()
            },
            payload: payload.clone().into(),
        }
        .marshal()
        .map_err(|error| MediaError::Transport(error.to_string()))?;
        assert_eq!(packet.encode()?.as_ref(), &reference[..], "round {round}");
    }
    Ok(())
}
