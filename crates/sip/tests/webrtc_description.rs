use rusterisk_config::Codec;
use rusterisk_sip::{Session, SessionSecurity, SipError, WebRtcDescription};

#[path = "support/webrtc.rs"]
mod descriptions;
use descriptions::offer;

#[test]
fn browser_descriptions_preserve_ice_security_and_cannot_generate_plaintext_answers(
) -> Result<(), SipError> {
    let source = offer();
    let session = Session::parse(&source, &[Codec::Pcmu, Codec::Opus])?;
    assert_eq!(session.codec, Codec::Pcmu);
    assert_eq!(session.remote, "0.0.0.0:9".parse()?);
    assert!(
        matches!(&session.security, SessionSecurity::WebRtc(description) if description.as_str() == source)
    );
    assert!(!format!("{session:?}").contains("PrivateIcePassword"));
    assert!(matches!(
        session.answer("127.0.0.1:10000".parse()?, 7),
        Err(SipError::Unsupported)
    ));
    Ok(())
}

#[test]
fn missing_or_malformed_media_security_is_rejected_at_the_description_boundary() {
    for (valid, invalid) in [
        ("a=ice-ufrag:peer", "a=ice-ufrag:p"),
        ("a=ice-pwd:PrivateIcePasswordForPeer22", "a=ice-pwd:short"),
        ("a=fingerprint:sha-256 ", "a=fingerprint:sha-1 "),
        ("a=rtcp-mux\r\n", "a=rtcp-mux-invalid\r\n"),
        ("a=setup:actpass", "a=setup:holdconn"),
        ("a=mid:0", "a=mid:"),
    ] {
        assert!(
            WebRtcDescription::parse(&offer().replace(valid, invalid)).is_err(),
            "{valid}"
        );
    }
    for required in [
        "a=ice-ufrag:",
        "a=ice-pwd:",
        "a=fingerprint:",
        "a=rtcp-mux",
        "a=setup:",
        "a=mid:",
    ] {
        let source = offer()
            .lines()
            .filter(|line| !line.starts_with(required))
            .collect::<Vec<_>>()
            .join("\r\n");
        assert!(WebRtcDescription::parse(&source).is_err(), "{required}");
    }
    assert!(matches!(
        WebRtcDescription::parse(&"x".repeat(16385)),
        Err(SipError::Capacity)
    ));
}
