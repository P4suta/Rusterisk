use rusterisk_config::Codec;
use rusterisk_media::MediaError;
use rusterisk_server::{
    rtp::SocketPair,
    webrtc::{WebRtcOffer, WebRtcSession},
};
use rusterisk_sip::Session;
use std::{net::Ipv4Addr, time::Instant};
#[path = "webrtc_telephone/duplex.rs"]
mod duplex;

fn failure(error: impl std::fmt::Display) -> MediaError {
    MediaError::Transport(error.to_string())
}

fn sockets() -> Result<SocketPair, MediaError> {
    SocketPair::bind(Ipv4Addr::LOCALHOST.into(), 0, 0).map_err(failure)
}

#[tokio::test]
async fn secure_audio_negotiates_telephone_events_at_the_audio_clock() -> Result<(), MediaError> {
    for (codec, rate) in [
        (Codec::Pcmu, 8000),
        (Codec::Pcma, 8000),
        (Codec::Opus, 48000),
    ] {
        let socket = sockets()?;
        let local = socket.local_addr().map_err(failure)?;
        let offer = WebRtcOffer::new(socket, local, &[codec], Instant::now())?;
        let offered = Session::parse(offer.description(), &[codec]).map_err(failure)?;
        let telephone = offered
            .telephone
            .ok_or_else(|| MediaError::Transport("Expected negotiated telephone events".into()))?;
        assert_eq!(telephone.rate(), rate);
        assert_eq!(telephone.events(), u16::MAX);
        let socket = sockets()?;
        let local = socket.local_addr().map_err(failure)?;
        let (_session, description) =
            WebRtcSession::answer(socket, local, offer.description(), &[codec], Instant::now())?;
        let answer = Session::parse(&description, &[codec]).map_err(failure)?;
        assert_eq!(answer.telephone, offered.telephone);
        offer.accept_answer(&description)?;
    }
    Ok(())
}

#[tokio::test]
async fn sparse_event_sets_survive_the_media_adapter_without_widening_the_answer(
) -> Result<(), MediaError> {
    let socket = sockets()?;
    let local = socket.local_addr().map_err(failure)?;
    let offer = WebRtcOffer::new(socket, local, &[Codec::Pcmu], Instant::now())?;
    let description = offer
        .description()
        .replace("a=fmtp:101 0-15", "a=fmtp:101 0-3,6-15");
    let socket = sockets()?;
    let local = socket.local_addr().map_err(failure)?;
    let (_session, answer) =
        WebRtcSession::answer(socket, local, &description, &[Codec::Pcmu], Instant::now())?;
    let session = Session::parse(&answer, &[Codec::Pcmu]).map_err(failure)?;
    let telephone = session
        .telephone
        .ok_or_else(|| MediaError::Transport("Expected sparse telephone events".into()))?;
    assert_eq!(telephone.events(), 0xffcf);
    offer.accept_answer(&answer)?;
    Ok(())
}
