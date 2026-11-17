use crate::{registration_responses, stream, OracleError, MAX_INPUT};
use rusterisk_config::{Codec, Config};
use rusterisk_sip::{
    Direction, Engine, Session, SessionSecurity, SipStreamDecoder, StreamFramingError,
    WebRtcDescription, MAX_MESSAGE,
};
use std::net::SocketAddr;

pub fn sip(bytes: &[u8]) -> Result<usize, OracleError> {
    if bytes.len() > MAX_INPUT {
        return Err(OracleError::InputCapacity);
    }
    let mut bulk = SipStreamDecoder::new();
    let expected = stream::read(bytes, bytes.len(), |input| bulk.push(input))?;
    let empty = bulk.is_empty();
    let end = bulk.finish();
    if bulk.push(b"") != Err(StreamFramingError::Closed) {
        return Err(OracleError::StreamChanged);
    }
    for chunk in [
        1,
        2,
        3,
        7,
        17,
        1 + usize::from(bytes.first().copied().unwrap_or(0)),
    ] {
        let mut decoder = SipStreamDecoder::new();
        let actual = stream::read(bytes, chunk, |input| decoder.push(input))?;
        if actual != expected
            || decoder.is_empty() != empty
            || decoder.finish() != end
            || decoder.push(b"") != Err(StreamFramingError::Closed)
        {
            return Err(OracleError::StreamChanged);
        }
    }
    let mut accepted = expected.messages.len() + registration_responses(bytes)?;
    let config = Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[fuzzer]\ntype=user\npassword=fuzz-password\n",
        "",
        "",
        "",
    )
    .map_err(|_| OracleError::SettingsChanged)?;
    let mut engine = Engine::new(config, [7; 32]);
    if let Ok(output) = engine.receive(bytes, SocketAddr::from(([127, 0, 0, 1], 5060)), 0) {
        if output
            .datagrams
            .iter()
            .any(|datagram| datagram.bytes.len() > MAX_MESSAGE)
        {
            return Err(OracleError::SignalingCapacity);
        }
        accepted += 1;
    }
    let Ok(source) = std::str::from_utf8(bytes) else {
        return Ok(accepted);
    };
    let allowed = [Codec::Pcmu, Codec::Pcma, Codec::Opus];
    if let Ok(session) = Session::parse(source, &allowed) {
        if matches!(session.security, SessionSecurity::Rtp) {
            let local = SocketAddr::from(([127, 0, 0, 1], 9000));
            let answer = session
                .answer(local, 7)
                .map_err(|_| OracleError::SessionEncoding)?;
            let parsed =
                Session::parse(&answer, &allowed).map_err(|_| OracleError::SessionChanged)?;
            let direction = match session.direction {
                Direction::SendRecv => Direction::SendRecv,
                Direction::SendOnly => Direction::RecvOnly,
                Direction::RecvOnly => Direction::SendOnly,
                Direction::Inactive => Direction::Inactive,
            };
            if parsed.remote != local
                || parsed.codec != session.codec
                || parsed.payload_type != session.payload_type
                || parsed.telephone != session.telephone
                || parsed.direction != direction
                || parsed.security != SessionSecurity::Rtp
            {
                return Err(OracleError::SessionChanged);
            }
        }
        accepted += 1;
    }
    if let Ok(description) = WebRtcDescription::parse(source) {
        if description.as_str() != source {
            return Err(OracleError::SessionChanged);
        }
        accepted += 1;
    }
    Ok(accepted)
}
