use rusterisk_core::{Call, CallEvent, ConnectionClaim, CoreError};

#[test]
fn a_connection_claim_is_exclusive_for_the_call_generation() -> Result<(), CoreError> {
    let call = Call::outgoing(7);
    assert_eq!(call.connection_claim(), ConnectionClaim::Available);
    let claimed = call.claim_connection()?;
    assert_eq!(claimed.generation(), 7);
    assert_eq!(claimed.connection_claim(), ConnectionClaim::Claimed);
    assert_eq!(
        claimed.claim_connection(),
        Err(CoreError::ConnectionClaimed)
    );
    assert_eq!(
        claimed.apply(CallEvent::Answer).call().connection_claim(),
        ConnectionClaim::Claimed
    );
    Ok(())
}

#[test]
fn terminal_events_release_claims_and_late_claims_do_not_revive_calls() -> Result<(), CoreError> {
    for call in [Call::incoming(1), Call::incoming(2).claim_connection()?] {
        for event in [CallEvent::Cancel, CallEvent::Hangup, CallEvent::Timeout] {
            let ended = call.apply(event).call();
            assert_eq!(ended.connection_claim(), ConnectionClaim::Released);
            assert_eq!(ended.claim_connection(), Err(CoreError::Ended));
            assert_eq!(ended.apply(CallEvent::Answer).call(), ended);
        }
    }
    Ok(())
}
