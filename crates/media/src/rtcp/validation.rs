use crate::{ControlKind, MediaError, MAX_PACKET};
use rtcp_types::RtcpPacketParser;

fn failure(error: rtcp_types::RtcpParseError) -> MediaError {
    MediaError::Control(error.to_string())
}

pub(crate) fn inspect_rtcp(bytes: &[u8]) -> Result<Vec<ControlKind>, MediaError> {
    if bytes.len() > MAX_PACKET {
        return Err(MediaError::Capacity);
    }
    let _compound = rtcp_types::Compound::parse(bytes).map_err(failure)?;
    let mut remaining = bytes;
    let mut kinds = Vec::with_capacity(bytes.len() / 4);
    while !remaining.is_empty() {
        let [flags, _kind, high, low, ..] = remaining else {
            return Err(MediaError::InvalidPacket);
        };
        if flags >> 6 != 2 {
            return Err(MediaError::InvalidPacket);
        }
        let length = (usize::from(u16::from_be_bytes([*high, *low])) + 1) * 4;
        let (packet, rest) = remaining
            .split_at_checked(length)
            .ok_or(MediaError::InvalidPacket)?;
        kinds.push(inspect_packet(packet)?);
        remaining = rest;
    }
    Ok(kinds)
}

fn inspect_packet(packet: &[u8]) -> Result<ControlKind, MediaError> {
    if packet.len() > MAX_PACKET {
        return Err(MediaError::Capacity);
    }
    let [flags, kind, _high, _low, ..] = packet else {
        return Err(MediaError::InvalidPacket);
    };
    let padding = if flags & 0x20 == 0 {
        0
    } else {
        let count = usize::from(*packet.last().ok_or(MediaError::InvalidPacket)?);
        if count == 0 || count > packet.len() - 4 {
            return Err(MediaError::InvalidPacket);
        }
        count
    };
    let body = &packet[4..packet.len() - padding];
    let count = usize::from(flags & 0x1f);
    if *kind == 202 {
        inspect_descriptions(body, count)?;
        return Ok(ControlKind::Description);
    }
    let minimum = match kind {
        200 => 24 + count * 24,
        201 => 4 + count * 24,
        203 => count * 4,
        204..=206 => 8,
        207 => 4,
        _ => 0,
    };
    if body.len() < minimum {
        return Err(MediaError::InvalidPacket);
    }
    if *kind == 203 {
        inspect_goodbye(body, count)?;
    }
    let mut scratch = [0; MAX_PACKET];
    let unpadded = if padding == 0 {
        packet
    } else {
        let size = packet.len() - padding;
        if !size.is_multiple_of(4) {
            return Err(MediaError::InvalidPacket);
        }
        scratch[..size].copy_from_slice(&packet[..size]);
        scratch[0] &= !0x20;
        scratch[2..4].copy_from_slice(&((size / 4 - 1) as u16).to_be_bytes());
        &scratch[..size]
    };
    Ok(
        match rtcp_types::Packet::parse(unpadded).map_err(failure)? {
            rtcp_types::Packet::Sr(_) => ControlKind::Sender,
            rtcp_types::Packet::Rr(_) => ControlKind::Receiver,
            rtcp_types::Packet::Sdes(_) => ControlKind::Description,
            rtcp_types::Packet::Bye(_) => ControlKind::Goodbye,
            rtcp_types::Packet::App(_) => ControlKind::Application,
            rtcp_types::Packet::TransportFeedback(_) => ControlKind::TransportFeedback,
            rtcp_types::Packet::PayloadFeedback(_) => ControlKind::PayloadFeedback,
            rtcp_types::Packet::Xr(_) => ControlKind::Extended,
            rtcp_types::Packet::Unknown(_) => return Err(MediaError::Unsupported),
        },
    )
}

fn inspect_goodbye(body: &[u8], count: usize) -> Result<(), MediaError> {
    let reason = body.get(count * 4..).ok_or(MediaError::InvalidPacket)?;
    let Some(length) = reason.first() else {
        return Ok(());
    };
    let end = 1 + usize::from(*length);
    if end.div_ceil(4) * 4 != reason.len()
        || reason
            .get(end..)
            .ok_or(MediaError::InvalidPacket)?
            .iter()
            .any(|byte| *byte != 0)
    {
        return Err(MediaError::InvalidPacket);
    }
    Ok(())
}

fn inspect_descriptions(mut body: &[u8], count: usize) -> Result<(), MediaError> {
    let mut scratch = [0; MAX_PACKET];
    for _ in 0..count {
        let mut end = 4;
        loop {
            let kind = *body.get(end).ok_or(MediaError::InvalidPacket)?;
            end += 1;
            if kind == 0 {
                break;
            }
            let length = usize::from(*body.get(end).ok_or(MediaError::InvalidPacket)?);
            end += 1;
            let value = body
                .get(end..end + length)
                .ok_or(MediaError::InvalidPacket)?;
            if kind == 8 {
                let prefix = usize::from(*value.first().ok_or(MediaError::InvalidPacket)?);
                if prefix + 1 > value.len() {
                    return Err(MediaError::InvalidPacket);
                }
            }
            end += length;
        }
        let aligned = end.div_ceil(4) * 4;
        if body
            .get(end..aligned)
            .ok_or(MediaError::InvalidPacket)?
            .iter()
            .any(|byte| *byte != 0)
        {
            return Err(MediaError::InvalidPacket);
        }
        let (chunk, rest) = body
            .split_at_checked(aligned)
            .ok_or(MediaError::InvalidPacket)?;
        let parsed = parse_chunk(chunk, &mut scratch)?;
        if parsed.chunks().count() != 1 {
            return Err(MediaError::InvalidPacket);
        }
        body = rest;
    }
    if !body.is_empty() {
        return Err(MediaError::InvalidPacket);
    }
    Ok(())
}

fn parse_chunk<'a>(
    chunk: &[u8],
    scratch: &'a mut [u8; MAX_PACKET],
) -> Result<rtcp_types::Sdes<'a>, MediaError> {
    let size = chunk.len().checked_add(4).ok_or(MediaError::Capacity)?;
    if size > scratch.len() {
        return Err(MediaError::Capacity);
    }
    if chunk.len() < 8 || !chunk.len().is_multiple_of(4) {
        return Err(MediaError::InvalidPacket);
    }
    scratch[..2].copy_from_slice(&[0x81, 202]);
    scratch[2..4].copy_from_slice(&((size / 4 - 1) as u16).to_be_bytes());
    scratch[4..size].copy_from_slice(chunk);
    rtcp_types::Sdes::parse(&scratch[..size]).map_err(failure)
}

#[cfg(test)]
mod tests {
    use super::parse_chunk;
    use crate::{MediaError, MAX_PACKET};

    #[test]
    fn borrowed_chunks_keep_original_source_identifiers_and_item_values() -> Result<(), MediaError>
    {
        for source in [0_u32, 7, 0x00112233, u32::MAX] {
            let mut chunk = [0; 12];
            chunk[..4].copy_from_slice(&source.to_be_bytes());
            chunk[4..].copy_from_slice(&[1, 4, b'c', b'a', b'l', b'l', 0, 0]);
            let mut scratch = [0; MAX_PACKET];
            let parsed = parse_chunk(&chunk, &mut scratch)?;
            let description = parsed.chunks().next().ok_or(MediaError::InvalidPacket)?;
            assert_eq!(description.ssrc(), source);
            let item = description
                .items()
                .next()
                .ok_or(MediaError::InvalidPacket)?;
            assert_eq!(item.type_(), 1);
            assert_eq!(item.value(), &chunk[6..10]);
        }
        Ok(())
    }

    #[test]
    fn oversized_chunks_leave_the_fixed_scratch_untouched() {
        let mut scratch = [0xa5; MAX_PACKET];
        assert_eq!(
            parse_chunk(&[0; MAX_PACKET], &mut scratch).err(),
            Some(MediaError::Capacity)
        );
        assert_eq!(scratch, [0xa5; MAX_PACKET]);
    }
}
