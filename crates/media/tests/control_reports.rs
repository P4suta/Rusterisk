use rusterisk_media::{goodbye_reports, sender_reports, ControlKind, MediaError, SourceReport};

#[test]
fn distinct_clock_reports_keep_counters_and_a_common_name_in_one_compound_packet(
) -> Result<(), MediaError> {
    let sources = [
        SourceReport {
            ssrc: 7,
            timestamp: 48000,
            packets: 50,
            octets: 10000,
        },
        SourceReport {
            ssrc: 8,
            timestamp: 8000,
            packets: 6,
            octets: 24,
        },
    ];
    let bytes = sender_reports(&sources, 12345, "call")?;
    assert_eq!(
        rusterisk_media::inspect_rtcp(&bytes)?,
        [
            ControlKind::Sender,
            ControlKind::Sender,
            ControlKind::Description,
            ControlKind::Description
        ]
    );
    let mut compound = rtcp_types::Compound::parse(&bytes)
        .map_err(|error| MediaError::Control(error.to_string()))?;
    for expected in sources {
        let Some(Ok(rtcp_types::Packet::Sr(report))) = compound.next() else {
            return Err(MediaError::InvalidPacket);
        };
        assert_eq!(report.ssrc(), expected.ssrc);
        assert_eq!(report.ntp_timestamp(), 12345);
        assert_eq!(report.rtp_timestamp(), expected.timestamp);
        assert_eq!(report.packet_count(), expected.packets);
        assert_eq!(report.octet_count(), expected.octets);
    }
    for source in sources {
        let Some(Ok(rtcp_types::Packet::Sdes(description))) = compound.next() else {
            return Err(MediaError::InvalidPacket);
        };
        assert_eq!(description.chunks().count(), 1);
        let chunk = description
            .chunks()
            .next()
            .ok_or(MediaError::InvalidPacket)?;
        assert_eq!(chunk.ssrc(), source.ssrc);
        let item = chunk.items().next().ok_or(MediaError::InvalidPacket)?;
        assert_eq!(item.type_(), 1);
        assert_eq!(item.value(), b"call");
    }
    let closing = goodbye_reports(&sources, 12345, "call")?;
    assert_eq!(
        rusterisk_media::inspect_rtcp(&closing)?,
        [
            ControlKind::Sender,
            ControlKind::Sender,
            ControlKind::Description,
            ControlKind::Description,
            ControlKind::Goodbye
        ]
    );
    let compound = rtcp_types::Compound::parse(&closing)
        .map_err(|error| MediaError::Control(error.to_string()))?;
    let Some(Ok(rtcp_types::Packet::Bye(bye))) = compound.last() else {
        return Err(MediaError::InvalidPacket);
    };
    assert_eq!(bye.ssrcs().collect::<Vec<_>>(), [7, 8]);
    assert!(sender_reports(&[], 0, "call").is_err());
    assert!(sender_reports(&[sources[0], sources[0]], 0, "call").is_err());
    assert!(sender_reports(&[sources[0]; 5], 0, "call").is_err());
    Ok(())
}
