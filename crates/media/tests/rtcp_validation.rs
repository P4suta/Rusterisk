use rtcp_types::{RtcpPacketParser, RtcpPacketParserExt};
use rusterisk_media::{inspect_rtcp, sender_report, ControlKind, MediaError, MAX_PACKET};

fn descriptions(count: u8, second: Option<u32>) -> Vec<u8> {
    let mut packet = Vec::with_capacity(36);
    packet.extend_from_slice(&[0x80, 201, 0, 1, 0x11, 0x22, 0x33, 0x44]);
    let size = if second.is_some() { 6_u16 } else { 3_u16 };
    packet.extend_from_slice(&[0x80 | count, 202]);
    packet.extend_from_slice(&size.to_be_bytes());
    packet.extend_from_slice(&[0x11, 0x22, 0x33, 0x44, 1, 2, b'a', b'b', 0, 0, 0, 0]);
    if let Some(second) = second {
        packet.extend_from_slice(&second.to_be_bytes());
        packet.extend_from_slice(&[1, 2, b'c', b'd', 0, 0, 0, 0]);
    }
    packet
}

#[test]
fn multiple_description_chunks_keep_leading_zero_source_identifiers() {
    for source in [0, 7, 0x00010000, 0x00112233, 0x11223344] {
        assert_eq!(
            inspect_rtcp(&descriptions(2, Some(source))),
            Ok([ControlKind::Receiver, ControlKind::Description]
                .into_iter()
                .collect()),
            "source: {source:08x}"
        );
    }
}

#[test]
fn description_counts_match_the_chunks_inside_the_packet_length() {
    for (count, second) in [(0, None), (2, None), (1, Some(0x11223344))] {
        assert!(
            inspect_rtcp(&descriptions(count, second)).is_err(),
            "declared chunks: {count}; second source: {second:?}"
        );
    }
}

#[test]
fn compound_padding_cannot_exceed_the_final_packet_body() {
    for padding in [0, 5, 8, 255] {
        let packet = [0xa0, 201, 0, 1, 0x11, 0x22, 0x33, padding];
        assert!(inspect_rtcp(&packet).is_err(), "padding: {padding}");
    }
}

#[test]
fn goodbye_counts_use_all_five_bits_and_exclude_the_padding_flag() -> Result<(), MediaError> {
    for count in [0_u8, 1, 15, 16, 31] {
        for padded in [false, true] {
            let size = 4 + usize::from(count) * 4 + usize::from(padded) * 4;
            let mut goodbye = Vec::with_capacity(size);
            goodbye.extend_from_slice(&[0x80 | count | if padded { 0x20 } else { 0 }, 203]);
            goodbye.extend_from_slice(&((size / 4 - 1) as u16).to_be_bytes());
            for source in 0..u32::from(count) {
                goodbye.extend_from_slice(&source.to_be_bytes());
            }
            if padded {
                goodbye.extend_from_slice(&[0, 0, 0, 4]);
            }
            let parsed = rtcp_types::Bye::parse(&goodbye)
                .map_err(|error| MediaError::Control(error.to_string()))?;
            assert_eq!(parsed.count(), count);
            assert_eq!(
                parsed.ssrcs().collect::<Vec<_>>(),
                (0..u32::from(count)).collect::<Vec<_>>()
            );
            let mut compound = sender_report(0x11223344, 0, 0, 0, 0, "call")?;
            compound.extend_from_slice(&goodbye);
            assert_eq!(
                inspect_rtcp(&compound)?,
                [
                    ControlKind::Sender,
                    ControlKind::Description,
                    ControlKind::Goodbye
                ]
            );
        }
    }
    Ok(())
}

#[test]
fn goodbye_sources_cannot_be_supplied_by_packet_padding() {
    let packet = [
        0x80, 201, 0, 1, 0x11, 0x22, 0x33, 0x44, 0xa1, 203, 0, 1, 0, 0, 0, 4,
    ];
    assert!(inspect_rtcp(&packet).is_err());
}

#[test]
fn description_terminators_end_at_the_chunk_alignment_boundary() -> Result<(), MediaError> {
    let missing_end = [
        0x80, 201, 0, 1, 0x11, 0x22, 0x33, 0x44, 0x81, 202, 0, 2, 0x11, 0x22, 0x33, 0x44, 1, 2,
        b'a', b'b',
    ];
    assert!(inspect_rtcp(&missing_end).is_err());
    let mut surplus = descriptions(1, None);
    surplus[10..12].copy_from_slice(&4_u16.to_be_bytes());
    surplus.extend_from_slice(&[0x11, 0x22, 0x33, 0x44]);
    assert!(inspect_rtcp(&surplus).is_err());
    let mut nonzero_padding = descriptions(1, None);
    *nonzero_padding
        .last_mut()
        .ok_or(MediaError::InvalidPacket)? = 1;
    assert!(inspect_rtcp(&nonzero_padding).is_err());
    Ok(())
}

#[test]
fn private_item_prefixes_stay_inside_the_declared_item_length() {
    let packet = [
        0x80, 201, 0, 1, 0x11, 0x22, 0x33, 0x44, 0x81, 202, 0, 3, 0x11, 0x22, 0x33, 0x44, 1, 2,
        b'a', b'b', 8, 1, 1, 0,
    ];
    assert!(inspect_rtcp(&packet).is_err());
}

#[test]
fn layout_limits_and_nonfinal_padding_are_rejected_before_chunk_parsing() {
    assert_eq!(
        inspect_rtcp(&[0; MAX_PACKET + 1]),
        Err(MediaError::Capacity)
    );
    let packet = [
        0xa0, 201, 0, 2, 0x11, 0x22, 0x33, 0x44, 0, 0, 0, 4, 0x80, 201, 0, 1, 0x11, 0x22, 0x33,
        0x44,
    ];
    assert!(inspect_rtcp(&packet).is_err());
}

#[test]
fn final_description_padding_stays_outside_all_counted_chunks() {
    let mut packet = descriptions(2, Some(7));
    packet[8] |= 0x20;
    packet[10..12].copy_from_slice(&7_u16.to_be_bytes());
    packet.extend_from_slice(&[0, 0, 0, 4]);
    assert_eq!(
        inspect_rtcp(&packet),
        Ok([ControlKind::Receiver, ControlKind::Description]
            .into_iter()
            .collect())
    );
}

#[test]
fn final_extended_report_padding_stays_outside_its_blocks() -> Result<(), MediaError> {
    let extended = [
        0xa0, 207, 0, 5, 0, 0, 0, 7, 4, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 4,
    ];
    let mut compound = sender_report(7, 0, 0, 0, 0, "call")?;
    compound.extend_from_slice(&extended);
    assert_eq!(
        inspect_rtcp(&compound),
        Ok([
            ControlKind::Sender,
            ControlKind::Description,
            ControlKind::Extended
        ]
        .into_iter()
        .collect())
    );
    Ok(())
}

#[test]
fn goodbye_reasons_and_alignment_cannot_read_rtcp_padding() -> Result<(), MediaError> {
    let cases: &[&[u8]] = &[
        &[0xa1, 203, 0, 3, 0, 0, 0, 7, 6, b'a', b'b', b'c', 0, 0, 0, 4],
        &[0x81, 203, 0, 2, 0, 0, 0, 7, 1, b'a', 0, 1],
    ];
    for goodbye in cases {
        let mut compound = sender_report(7, 0, 0, 0, 0, "call")?;
        compound.extend_from_slice(goodbye);
        assert!(inspect_rtcp(&compound).is_err());
    }
    Ok(())
}
