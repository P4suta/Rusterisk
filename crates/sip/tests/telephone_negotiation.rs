use rusterisk_config::Codec;
use rusterisk_core::DtmfDigit;
use rusterisk_sip::{DtmfFormat, Session, SipError};

fn offer(attributes: &str) -> String {
    format!("v=0\r\nc=IN IP4 127.0.0.1\r\nm=audio 10002 RTP/AVP 0 111 101 102\r\na=rtpmap:111 opus/48000/2\r\na=rtpmap:101 telephone-event/8000\r\na=rtpmap:102 telephone-event/48000\r\n{attributes}")
}

#[test]
fn telephone_negotiation_prefers_the_audio_clock_and_preserves_the_peer_event_set(
) -> Result<(), SipError> {
    let source = offer("a=fmtp:101 0-16\r\na=fmtp:102 0-3,10-11,15\r\n");
    let session = Session::parse(&source, &[Codec::Opus, Codec::Pcmu])?;
    let telephone = session.telephone.ok_or(SipError::Unsupported)?;
    assert_eq!(telephone.payload_type(), 102);
    assert_eq!(telephone.rate(), 48000);
    for digit in [
        DtmfDigit::Zero,
        DtmfDigit::Star,
        DtmfDigit::Hash,
        DtmfDigit::D,
    ] {
        assert!(telephone.supports(digit));
    }
    for digit in [DtmfDigit::Four, DtmfDigit::A, DtmfDigit::B, DtmfDigit::C] {
        assert!(!telephone.supports(digit));
    }
    let answer = session.answer("127.0.0.1:10004".parse()?, 1)?;
    assert!(answer.contains("a=rtpmap:102 telephone-event/48000\r\n"));
    assert!(!answer.contains("telephone-event/8000"));
    assert_eq!(
        Session::parse(&answer, &[Codec::Opus])?.telephone,
        session.telephone
    );
    let narrow = Session::parse(&source, &[Codec::Pcmu])?;
    assert_eq!(narrow.telephone, Some(DtmfFormat::new(101, 8000)?));
    Ok(())
}

#[test]
fn absent_event_parameters_use_only_dtmf_and_malformed_ranges_are_refused() -> Result<(), SipError>
{
    let session = Session::parse(&offer(""), &[Codec::Opus])?;
    assert_eq!(session.telephone, Some(DtmfFormat::new(102, 48000)?));
    for malformed in ["3-1", "0-256", "0-3,", "0-3, 4", "x", "0--3"] {
        assert!(Session::parse(
            &offer(&format!("a=fmtp:102 {malformed}\r\n")),
            &[Codec::Opus]
        )
        .is_err());
    }
    let source = offer("a=fmtp:101 16\r\na=fmtp:102 16\r\n");
    assert!(Session::parse(&source, &[Codec::Opus])?.telephone.is_none());
    assert!(DtmfFormat::new(128, 8000).is_err());
    assert!(DtmfFormat::new(101, 16000).is_err());
    Ok(())
}

#[test]
fn all_negotiated_clocks_retain_their_masks_and_canonical_parameters() -> Result<(), SipError> {
    let formats = DtmfFormat::parse_all(&offer("a=fmtp:101 0-16\r\na=fmtp:102 0-3,10-11,15\r\n"))?;
    assert_eq!(formats.len(), 2);
    assert_eq!(formats[0].payload_type(), 101);
    assert_eq!(formats[0].rate(), 8000);
    assert_eq!(formats[0].parameters(), "0-15");
    assert_eq!(formats[1].payload_type(), 102);
    assert_eq!(formats[1].rate(), 48000);
    assert_eq!(formats[1].parameters(), "0-3,10-11,15");
    assert!(DtmfFormat::parse_all(&" ".repeat(16385)).is_err());
    let unoffered = offer("a=fmtp:101 0-3\r\n").replace("111 101 102", "111");
    assert!(DtmfFormat::parse_all(&unoffered)?.is_empty());
    Ok(())
}
