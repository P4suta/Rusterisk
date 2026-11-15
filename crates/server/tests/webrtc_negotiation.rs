use rusterisk_config::Codec;
use rusterisk_media::MediaError;
use rusterisk_server::{
    rtp::SocketPair,
    webrtc::{WebRtcOffer, WebRtcSession},
};
use std::{net::Ipv4Addr, time::Instant};

fn failure(error: impl std::fmt::Display) -> MediaError {
    MediaError::Transport(error.to_string())
}
fn sockets() -> Result<SocketPair, MediaError> {
    SocketPair::bind(Ipv4Addr::LOCALHOST.into(), 0, 0).map_err(failure)
}

#[tokio::test]
async fn audio_negotiation_preserves_ice_and_dtls_without_exposing_credentials(
) -> Result<(), MediaError> {
    let left = sockets()?;
    let left_address = left.local_addr().map_err(failure)?;
    let right = sockets()?;
    let right_address = right.local_addr().map_err(failure)?;
    let offer = WebRtcOffer::new(left, left_address, &[Codec::Pcmu], Instant::now())
        .map_err(|error| failure(format!("offer: {error}")))?;
    for required in [
        "UDP/TLS/RTP/SAVPF",
        "a=ice-ufrag:",
        "a=ice-pwd:",
        "a=fingerprint:sha-256 ",
        "a=rtcp-mux",
    ] {
        assert!(offer.description().contains(required), "{required}");
    }
    assert!(!offer.description().contains("m=video"));
    let password = offer
        .description()
        .lines()
        .find_map(|line| line.strip_prefix("a=ice-pwd:"))
        .ok_or(MediaError::InvalidPacket)?;
    assert!(password.len() >= 43);
    assert!(!format!("{offer:?}").contains(password));
    let (_right, answer) = WebRtcSession::answer(
        right,
        right_address,
        offer.description(),
        &[Codec::Pcmu],
        Instant::now(),
    )
    .map_err(|error| failure(format!("answer: {error}")))?;
    assert!(answer.contains("a=rtcp-mux"));
    let _left = offer
        .accept_answer(&answer)
        .map_err(|error| failure(format!("accept answer: {error}")))?;
    Ok(())
}

#[tokio::test]
async fn plaintext_or_oversized_descriptions_are_rejected_before_media_attachment(
) -> Result<(), MediaError> {
    for (description, expected) in [("v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=plain\r\nt=0 0\r\nm=audio 5000 RTP/AVP 0\r\nc=IN IP4 127.0.0.1\r\na=rtpmap:0 PCMU/8000\r\n".to_owned(), MediaError::Unsupported), ("x".repeat(16385), MediaError::Capacity)] {
        let socket = sockets()?;
        let address = socket.local_addr().map_err(failure)?;
        assert!(matches!(WebRtcSession::answer(socket, address, &description, &[Codec::Pcmu], Instant::now()), Err(error) if error == expected));
    }
    Ok(())
}

#[tokio::test]
async fn a_malformed_mux_attribute_cannot_enable_unnegotiated_multiplexing(
) -> Result<(), MediaError> {
    let left = sockets()?;
    let left_address = left.local_addr().map_err(failure)?;
    let offer = WebRtcOffer::new(left, left_address, &[Codec::Pcmu], Instant::now())?;
    let malformed = offer
        .description()
        .replace("a=rtcp-mux\r\n", "a=rtcp-mux-invalid\r\n");
    let right = sockets()?;
    let right_address = right.local_addr().map_err(failure)?;
    assert!(matches!(
        WebRtcSession::answer(
            right,
            right_address,
            &malformed,
            &[Codec::Pcmu],
            Instant::now()
        ),
        Err(MediaError::InvalidPacket)
    ));
    Ok(())
}
