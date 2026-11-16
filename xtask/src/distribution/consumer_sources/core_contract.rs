use rusterisk_core::{
    Call, CallEvent, CallState, ConnectionClaim, MediaConnection, MediaState, Transition,
};

pub fn verify() -> Result<(), std::io::Error> {
    let ringing = Call::outgoing(17).apply(CallEvent::Ring);
    if ringing.state() != CallState::Ringing || ringing.transition() != Transition::Notify {
        return Err(std::io::Error::other("core ringing contract failed"));
    }
    let answered = ringing.call().apply(CallEvent::Answer);
    if answered.state() != CallState::Up || answered.transition() != Transition::Connect {
        return Err(std::io::Error::other("core answer contract failed"));
    }
    let ended = answered.call().apply(CallEvent::Hangup);
    if ended.state() != CallState::Ended
        || ended.transition() != Transition::Release
        || ended.call().media() != MediaState::Released
        || ended.call().media_connection() != MediaConnection::Released
        || ended.call().connection_claim() != ConnectionClaim::Released
        || ended.call().apply(CallEvent::Hangup).transition() != Transition::Ignore
    {
        return Err(std::io::Error::other(
            "core terminal cleanup contract failed",
        ));
    }
    Ok(())
}
