use super::{transaction, transmitted};
use rusterisk_sip::{Datagram, RegistrationResponse, RegistrationTransactionEvent, SipError};

pub(super) fn response(packet: &Datagram, status: u16, extra: &str) -> Result<Vec<u8>, SipError> {
    let source = std::str::from_utf8(&packet.bytes)?;
    let mut output = format!("SIP/2.0 {status} Response\r\n");
    for name in ["Via", "From", "To", "Call-ID", "CSeq"] {
        let line = source
            .lines()
            .find(|line| line.starts_with(&format!("{name}: ")))
            .ok_or(SipError::InvalidPacket)?;
        output.push_str(line);
        if name == "To" {
            output.push_str(";tag=registrar");
        }
        output.push_str("\r\n");
    }
    output.push_str(extra);
    output.push_str("Content-Length: 0\r\n\r\n");
    Ok(output.into_bytes())
}

pub(super) fn final_response(
    event: Option<RegistrationTransactionEvent>,
) -> Result<RegistrationResponse, SipError> {
    match event {
        Some(RegistrationTransactionEvent::Response(response)) => Ok(response),
        Some(
            RegistrationTransactionEvent::Provisional(_)
            | RegistrationTransactionEvent::TimedOut
            | RegistrationTransactionEvent::Transmit(_),
        )
        | None => Err(SipError::InvalidPacket),
    }
}

#[test]
fn response_identity_checks_peer_branch_sent_by_call_id_method_and_sequence() -> Result<(), SipError>
{
    let mut pending = transaction("udp", 0)?;
    let packet = transmitted(pending.poll(0)?)?;
    let valid = response(&packet, 200, "")?;
    assert!(pending
        .receive(&valid, "127.0.0.3:5070".parse()?, 100)?
        .is_none());
    let text = std::str::from_utf8(&valid)?;
    for wrong in [
        text.replace("branch=z9hG4bK", "branch=z9hG4bK-other"),
        text.replace("127.0.0.1:5068", "127.0.0.1:5069"),
        text.replace("Call-ID: reg-", "Call-ID: other-"),
        text.replace("CSeq: 7 REGISTER", "CSeq: 8 REGISTER"),
        text.replace("CSeq: 7 REGISTER", "CSeq: 7 INVITE"),
    ] {
        assert!(pending
            .receive(wrong.as_bytes(), packet.peer, 100)?
            .is_none());
    }
    assert_eq!(pending.next_deadline(), Some(500));
    let final_event = final_response(pending.receive(&valid, packet.peer, 101)?)?;
    assert_eq!(final_event.status(), 200);
    Ok(())
}

#[test]
fn provisional_responses_keep_timer_f_and_use_timer_t2_for_datagram_retries() -> Result<(), SipError>
{
    let mut pending = transaction("udp", 0)?;
    let packet = transmitted(pending.poll(0)?)?;
    assert!(matches!(
        pending.receive(&response(&packet, 100, "")?, packet.peer, 100)?,
        Some(RegistrationTransactionEvent::Provisional(100))
    ));
    assert_eq!(pending.next_deadline(), Some(500));
    assert!(pending.poll(499)?.is_none());
    assert_eq!(transmitted(pending.poll(500)?)?.bytes, packet.bytes);
    assert_eq!(pending.next_deadline(), Some(4500));
    transmitted(pending.poll(4500)?)?;
    assert_eq!(pending.next_deadline(), Some(8500));
    assert_eq!(pending.expires_at(), 32_000);
    assert!(matches!(
        pending.poll(32_000)?,
        Some(RegistrationTransactionEvent::TimedOut)
    ));
    Ok(())
}

#[test]
fn final_datagram_receipts_own_wire_identity_and_absorb_duplicates_until_timer_k(
) -> Result<(), SipError> {
    let mut pending = transaction("udp", 0)?;
    let packet = transmitted(pending.poll(0)?)?;
    let reply = response(&packet, 200, "Expires: 120\r\n")?;
    let received = final_response(pending.receive(&reply, packet.peer, 1000)?)?;
    assert_eq!(received.status(), 200);
    assert_eq!(received.identity(), pending.identity());
    assert_eq!(received.peer(), packet.peer);
    assert_eq!(
        received.header_values("expires").collect::<Vec<_>>(),
        ["120"]
    );
    assert_eq!(
        received.request_uri()?,
        "SIP:Provider.Example:05060;transport=UDP"
    );
    assert!(!pending.is_finished());
    assert_eq!(pending.next_deadline(), Some(6000));
    assert!(pending.receive(&reply, packet.peer, 2000)?.is_none());
    assert!(pending.poll(5999)?.is_none());
    assert!(pending.poll(6000)?.is_none());
    assert!(pending.is_finished());
    assert!(pending.poll(32_000)?.is_none());
    Ok(())
}

#[test]
fn reliable_responses_finish_without_timer_k_or_request_retransmission() -> Result<(), SipError> {
    for protocol in ["tcp", "tls"] {
        let mut pending = transaction(protocol, 0)?;
        let packet = transmitted(pending.poll(0)?)?;
        assert!(matches!(
            pending.receive(&response(&packet, 100, "")?, packet.peer, 100)?,
            Some(RegistrationTransactionEvent::Provisional(100))
        ));
        assert_eq!(pending.next_deadline(), Some(32_000));
        let final_event =
            final_response(pending.receive(&response(&packet, 403, "")?, packet.peer, 1000)?)?;
        assert_eq!(final_event.status(), 403);
        assert!(pending.is_finished());
        assert_eq!(pending.next_deadline(), None);
        assert!(pending.poll(32_000)?.is_none());
    }
    Ok(())
}

#[test]
fn unsent_cancelled_and_expired_transactions_do_not_accept_responses() -> Result<(), SipError> {
    let mut reference = transaction("udp", 0)?;
    let packet = transmitted(reference.poll(0)?)?;
    let reply = response(&packet, 200, "")?;
    let mut unsent = transaction("udp", 0)?;
    assert!(unsent.receive(&reply, packet.peer, 0)?.is_none());
    transmitted(unsent.poll(0)?)?;
    unsent.cancel();
    assert!(unsent.receive(&reply, packet.peer, 0)?.is_none());
    assert!(unsent
        .receive(b"malformed", packet.peer, u64::MAX)?
        .is_none());
    assert!(reference.receive(&reply, packet.peer, 32_000)?.is_none());
    assert!(matches!(
        reference.poll(32_000)?,
        Some(RegistrationTransactionEvent::TimedOut)
    ));
    assert!(reference.receive(&reply, packet.peer, 32_001)?.is_none());
    Ok(())
}

#[test]
fn malformed_or_ambiguous_responses_do_not_change_the_request_state() -> Result<(), SipError> {
    let mut pending = transaction("udp", 0)?;
    let packet = transmitted(pending.poll(0)?)?;
    let valid = response(&packet, 200, "")?;
    let text = std::str::from_utf8(&valid)?;
    let call_id = text
        .lines()
        .find(|line| line.starts_with("Call-ID: "))
        .ok_or(SipError::InvalidPacket)?;
    let duplicate_identity = response(&packet, 200, &format!("{call_id}\r\n"))?;
    let duplicate_sequence = response(&packet, 200, "CSeq: 7 REGISTER\r\n")?;
    for malformed in [
        text.replace("Content-Length: 0", "Content-Length: 1"),
        text.replace("\r\n", "\n"),
        format!("{text}trailing"),
        std::str::from_utf8(&duplicate_identity)?.into(),
        std::str::from_utf8(&duplicate_sequence)?.into(),
    ] {
        assert!(pending
            .receive(malformed.as_bytes(), packet.peer, 100)
            .is_err());
    }
    assert!(matches!(
        pending.receive(
            &b"x".repeat(rusterisk_sip::MAX_MESSAGE + 1),
            packet.peer,
            100
        ),
        Err(SipError::Capacity)
    ));
    assert_eq!(transmitted(pending.poll(500)?)?.bytes, packet.bytes);
    Ok(())
}

#[test]
fn datagrams_can_omit_content_length_while_reliable_responses_require_it() -> Result<(), SipError> {
    for protocol in ["udp", "tcp", "tls"] {
        let mut pending = transaction(protocol, 0)?;
        let packet = transmitted(pending.poll(0)?)?;
        let bytes = response(&packet, 200, "")?;
        let source = std::str::from_utf8(&bytes)?.replace("Content-Length: 0\r\n", "");
        let result = pending.receive(source.as_bytes(), packet.peer, 100);
        if protocol == "udp" {
            assert_eq!(final_response(result?)?.status(), 200);
        } else {
            assert!(matches!(result, Err(SipError::InvalidPacket)));
        }
    }
    Ok(())
}
