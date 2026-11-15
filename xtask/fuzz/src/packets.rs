use crate::{stream, OracleError, MAX_INPUT};
use rusterisk_media::{inspect_rtcp, AudioSocket, AudioSocketDecoder, RtpPacket, TelephoneEvent};

pub fn media(bytes: &[u8]) -> Result<usize, OracleError> {
    if bytes.len() > MAX_INPUT {
        return Err(OracleError::InputCapacity);
    }
    let mut bulk = AudioSocketDecoder::new();
    let expected = stream::read(bytes, bytes.len(), |input| bulk.push(input))?;
    let empty = bulk.is_empty();
    for chunk in [
        1,
        2,
        3,
        7,
        17,
        1 + usize::from(bytes.first().copied().unwrap_or(0)),
    ] {
        let mut decoder = AudioSocketDecoder::new();
        let actual = stream::read(bytes, chunk, |input| decoder.push(input))?;
        if actual != expected || decoder.is_empty() != empty {
            return Err(OracleError::StreamChanged);
        }
    }
    let mut accepted = expected.messages.len();
    if let Ok(message) = AudioSocket::parse(bytes) {
        let encoded = message.encode().map_err(|_| OracleError::AudioEncoding)?;
        if AudioSocket::parse(&encoded).as_ref() != Ok(&message) {
            return Err(OracleError::AudioChanged);
        }
        accepted += 1;
    }
    if let Ok(packet) = RtpPacket::parse(bytes) {
        let encoded = packet.encode().map_err(|_| OracleError::PacketEncoding)?;
        if RtpPacket::parse(&encoded).as_ref() != Ok(&packet) {
            return Err(OracleError::PacketChanged);
        }
        accepted += 1;
    }
    if let Ok(event) = TelephoneEvent::parse(bytes) {
        if TelephoneEvent::parse(&event.encode()).as_ref() != Ok(&event) {
            return Err(OracleError::TelephoneChanged);
        }
        accepted += 1;
    }
    if inspect_rtcp(bytes).is_ok() {
        accepted += 1;
    }
    Ok(accepted)
}
