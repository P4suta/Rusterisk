use super::{
    answered, cleanup_requests, established, forked, outbound, request, transaction_response,
};
use rsipstack::sip::{headers::untyped::ToTypedHeader, HeadersExt, Method};
use rusterisk_config::Protocol;
use rusterisk_sip::{ForkTeardownFailure, SipError, SipEvent};

#[test]
fn transaction_saturation_preserves_the_ack_winner_and_uncommitted_teardown() -> Result<(), SipError>
{
    let mut call = established(Protocol::Udp)?;
    let mut pending = Vec::with_capacity(128);
    for _ in 0..128 {
        let output =
            call.engine
                .send_info("outgoing", 7, rusterisk_core::DtmfDigit::One, 100, 2)?;
        let [packet] = output.datagrams.as_slice() else {
            return Err(SipError::InvalidPacket);
        };
        pending.push(packet.bytes.clone());
    }
    let response = forked(&call.invite, "other")?;
    let failed = call.engine.receive(&response, call.peer, 3)?;
    let [ack] = failed.datagrams.as_slice() else {
        return Err(SipError::InvalidPacket);
    };
    assert_eq!(request(&ack.bytes)?.method, Method::Ack);
    assert!(
        matches!(failed.events.as_slice(), [SipEvent::ForkTeardownFailed {
        id,
        generation: 7,
        failure: ForkTeardownFailure::TransactionCapacity,
    }] if id == "outgoing")
    );
    assert!(call.engine.active_session("outgoing", 7).is_some());
    let first = pending.first().ok_or(SipError::InvalidPacket)?;
    call.engine
        .receive(&transaction_response(first, 200)?, call.peer, 4)?;
    let recovered = call.engine.receive(&response, call.peer, 5)?;
    assert!(recovered.events.is_empty());
    cleanup_requests(&recovered.datagrams, "other")?;
    assert_eq!(recovered.datagrams[0].bytes, ack.bytes);
    assert!(call.engine.active_session("outgoing", 7).is_some());
    Ok(())
}

#[test]
fn a_pending_teardown_keeps_its_first_ack_and_target_until_transaction_capacity_recovers(
) -> Result<(), SipError> {
    let mut call = established(Protocol::Udp)?;
    let mut pending = Vec::with_capacity(128);
    for _ in 0..128 {
        let output =
            call.engine
                .send_info("outgoing", 7, rusterisk_core::DtmfDigit::One, 100, 2)?;
        let [packet] = output.datagrams.as_slice() else {
            return Err(SipError::InvalidPacket);
        };
        pending.push(packet.bytes.clone());
    }
    let response = forked(&call.invite, "other")?;
    let first = call.engine.receive(&response, call.peer, 3)?;
    let [original_ack] = first.datagrams.as_slice() else {
        return Err(SipError::InvalidPacket);
    };
    let original = request(&original_ack.bytes)?;
    assert_eq!(original.method, Method::Ack);
    assert_eq!(original.cseq_header()?.typed()?.seq, 1);
    assert!(matches!(
        first.events.as_slice(),
        [SipEvent::ForkTeardownFailed {
            failure: ForkTeardownFailure::TransactionCapacity,
            ..
        }]
    ));
    let changed = std::str::from_utf8(&response)?.replace(
        "Contact: <sip:phone@127.0.0.1:5061>",
        "Contact: <sip:changed@127.0.0.1:5061>",
    );
    assert_ne!(changed.as_bytes(), response.as_slice());
    let repeated = call.engine.receive(changed.as_bytes(), call.peer, 4)?;
    let [repeated_ack] = repeated.datagrams.as_slice() else {
        return Err(SipError::InvalidPacket);
    };
    assert!(
        repeated_ack.bytes == original_ack.bytes,
        "A changed Contact changed the pending fork acknowledgment"
    );
    assert!(matches!(
        repeated.events.as_slice(),
        [SipEvent::ForkTeardownFailed {
            failure: ForkTeardownFailure::TransactionCapacity,
            ..
        }]
    ));
    let first_pending = pending.first().ok_or(SipError::InvalidPacket)?;
    call.engine
        .receive(&transaction_response(first_pending, 200)?, call.peer, 5)?;
    let recovered = call.engine.receive(changed.as_bytes(), call.peer, 6)?;
    assert!(recovered.events.is_empty());
    cleanup_requests(&recovered.datagrams, "other")?;
    assert!(
        recovered.datagrams[0].bytes == original_ack.bytes,
        "Transaction capacity recovery changed the pending fork acknowledgment"
    );
    let bye = request(&recovered.datagrams[1].bytes)?;
    assert_eq!(bye.uri, original.uri);
    assert_eq!(bye.to_header()?.value(), original.to_header()?.value());
    assert_eq!(bye.cseq_header()?.typed()?.seq, 2);
    call.engine.receive(
        &transaction_response(&recovered.datagrams[1].bytes, 200)?,
        call.peer,
        7,
    )?;
    let settled = call.engine.receive(changed.as_bytes(), call.peer, 8)?;
    let [settled_ack] = settled.datagrams.as_slice() else {
        return Err(SipError::InvalidPacket);
    };
    assert!(
        settled_ack.bytes == original_ack.bytes,
        "BYE settlement changed the pending fork acknowledgment"
    );
    assert!(settled.events.is_empty());
    assert!(call.engine.active_session("outgoing", 7).is_some());
    Ok(())
}

#[test]
fn retained_teardown_capacity_is_shared_and_expires_without_suppressing_acknowledgments(
) -> Result<(), SipError> {
    let mut call = established(Protocol::Udp)?;
    for index in 0..128 {
        let now = 2 + index * 2;
        let output = call.engine.receive(
            &forked(&call.invite, &format!("branch-{index}"))?,
            call.peer,
            now,
        )?;
        cleanup_requests(&output.datagrams, &format!("branch-{index}"))?;
        assert!(output.events.is_empty());
        call.engine.receive(
            &transaction_response(&output.datagrams[1].bytes, 200)?,
            call.peer,
            now + 1,
        )?;
    }
    let mut other = outbound();
    other.id = "other-call".into();
    other.generation = 8;
    let output = call.engine.originate(other, 300)?;
    let [invite] = output.datagrams.as_slice() else {
        return Err(SipError::InvalidPacket);
    };
    call.engine
        .receive(&answered(&invite.bytes)?, call.peer, 301)?;
    let response = forked(&invite.bytes, "other")?;
    let failed = call.engine.receive(&response, call.peer, 302)?;
    let [ack] = failed.datagrams.as_slice() else {
        return Err(SipError::InvalidPacket);
    };
    assert_eq!(request(&ack.bytes)?.method, Method::Ack);
    assert!(
        matches!(failed.events.as_slice(), [SipEvent::ForkTeardownFailed {
        id,
        generation: 8,
        failure: ForkTeardownFailure::RetentionCapacity,
    }] if id == "other-call")
    );
    let first = call
        .engine
        .receive(&forked(&call.invite, "branch-0")?, call.peer, 303)?;
    let [repeated_ack] = first.datagrams.as_slice() else {
        return Err(SipError::InvalidPacket);
    };
    assert_eq!(request(&repeated_ack.bytes)?.method, Method::Ack);
    assert!(first.events.is_empty());
    let recovered = call.engine.receive(&response, call.peer, 32_001)?;
    assert!(recovered.events.is_empty());
    cleanup_requests(&recovered.datagrams, "other")?;
    assert_eq!(recovered.datagrams[0].bytes, ack.bytes);
    assert!(call.engine.active_session("outgoing", 7).is_some());
    assert!(call.engine.active_session("other-call", 8).is_some());
    Ok(())
}
