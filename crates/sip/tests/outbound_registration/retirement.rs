use super::{
    authentication::header,
    responses::{final_response, response},
    transaction, transmitted,
};
use rusterisk_sip::{
    Datagram, RegistrationRetirement, RegistrationRetirementReceipt, RegistrationRetirementStatus,
    RegistrationTransaction, RegistrationTransactionEvent, SipError, MAX_MESSAGE,
};

fn prepared(
    protocol: &str,
    now: u64,
) -> Result<(RegistrationTransaction, Datagram, RegistrationRetirement), SipError> {
    let mut pending = transaction(protocol, now)?;
    let packet = transmitted(pending.poll(now)?)?;
    let retirement = pending
        .prepare_retirement(now)?
        .ok_or(SipError::InvalidPacket)?;
    Ok((pending, packet, retirement))
}

#[test]
fn preparation_keeps_the_original_transaction_usable_until_the_caller_commits(
) -> Result<(), SipError> {
    for protocol in ["udp", "tcp", "tls"] {
        let (mut pending, packet, mut retirement) = prepared(protocol, 1000)?;
        let identity = pending.identity();
        let next = pending.next_deadline();
        assert!(matches!(
            pending.prepare_retirement(999),
            Err(SipError::InvalidPacket)
        ));
        assert_eq!(pending.identity(), identity);
        assert_eq!(pending.expires_at(), 33_000);
        assert_eq!(pending.next_deadline(), next);
        assert!(matches!(
            retirement.poll(1000)?,
            RegistrationRetirementStatus::Waiting
        ));
        assert!(pending.poll(1000)?.is_none());
        if protocol == "udp" {
            let retry = transmitted(pending.poll(1500)?)?;
            assert!(
                retry.bytes == packet.bytes,
                "Retirement changed request bytes"
            );
            assert_eq!(retry.peer, packet.peer);
        } else {
            assert!(pending.poll(1500)?.is_none());
        }
        let received =
            final_response(pending.receive(&response(&packet, 200, "")?, packet.peer, 1600)?)?;
        assert_eq!(received.status(), 200);
        assert!(pending.prepare_retirement(1600)?.is_none());
    }
    Ok(())
}

#[test]
fn unsent_final_cancelled_and_expired_requests_do_not_need_retirement_slots() -> Result<(), SipError>
{
    for protocol in ["udp", "tcp", "tls"] {
        let mut unsent = transaction(protocol, 1000)?;
        let identity = unsent.identity();
        assert!(unsent.prepare_retirement(1000)?.is_none());
        assert_eq!(unsent.identity(), identity);
        assert_eq!(unsent.next_deadline(), Some(1000));
        transmitted(unsent.poll(1000)?)?;
        assert!(unsent.prepare_retirement(1000)?.is_some());
        unsent.cancel();
        assert!(unsent.prepare_retirement(0)?.is_none());
        assert!(unsent.prepare_retirement(u64::MAX)?.is_none());

        let (mut expired, _packet, _retirement) = prepared(protocol, 1000)?;
        let next = expired.next_deadline();
        assert!(expired.prepare_retirement(33_000)?.is_none());
        assert_eq!(expired.next_deadline(), next);
        assert_eq!(expired.expires_at(), 33_000);
        assert!(matches!(
            expired.poll(33_000)?,
            Some(RegistrationTransactionEvent::TimedOut)
        ));
        assert!(expired.prepare_retirement(33_000)?.is_none());
    }
    Ok(())
}

#[test]
fn final_responses_release_retirement_after_original_cancellation_without_authentication_retry(
) -> Result<(), SipError> {
    for protocol in ["udp", "tcp", "tls"] {
        for status in [200, 299, 401, 407, 423, 503, 603, 699] {
            let (mut pending, packet, mut retirement) = prepared(protocol, 1000)?;
            pending.cancel();
            assert!(pending.poll(u64::MAX)?.is_none());
            assert_eq!(retirement.expires_at(), 33_000);
            assert!(matches!(
                retirement.receive(&response(&packet, status, "")?, packet.peer, 1100)?,
                RegistrationRetirementReceipt::Final(code) if code == status
            ));
            for now in [1100, 33_000, u64::MAX, 0] {
                assert!(matches!(
                    retirement.poll(now)?,
                    RegistrationRetirementStatus::Released
                ));
                assert!(matches!(
                    retirement.receive(b"malformed", packet.peer, now)?,
                    RegistrationRetirementReceipt::Unmatched
                ));
            }
            assert_eq!(retirement.expires_at(), 33_000);
        }
    }
    Ok(())
}

#[test]
fn provisional_and_unrelated_responses_preserve_the_original_deadline() -> Result<(), SipError> {
    for (protocol, token, other) in [
        ("udp", "UDP", "TCP"),
        ("tcp", "TCP", "TLS"),
        ("tls", "TLS", "UDP"),
    ] {
        let (_pending, packet, mut retirement) = prepared(protocol, 1000)?;
        let valid = response(&packet, 200, "")?;
        let text = std::str::from_utf8(&valid)?;
        assert!(matches!(
            retirement.receive(&valid, "127.0.0.3:5070".parse()?, 1100)?,
            RegistrationRetirementReceipt::Unmatched
        ));
        for changed in [
            text.replace("branch=z9hG4bK", "branch=z9hG4bK-other"),
            text.replace("127.0.0.1:5068", "127.0.0.1:5069"),
            text.replace("Call-ID: reg-", "Call-ID: other-"),
            text.replace("CSeq: 7 REGISTER", "CSeq: 8 REGISTER"),
            text.replace("CSeq: 7 REGISTER", "CSeq: 7 INVITE"),
            text.replacen(";tag=", ";tag=other-", 1),
            text.replace(&format!("SIP/2.0/{token}"), &format!("SIP/2.0/{other}")),
        ] {
            assert!(changed != text, "Response mutation did not change the wire");
            assert!(matches!(
                retirement.receive(changed.as_bytes(), packet.peer, 1100)?,
                RegistrationRetirementReceipt::Unmatched
            ));
        }
        assert!(matches!(
            retirement.receive(&packet.bytes, packet.peer, 1100)?,
            RegistrationRetirementReceipt::Unmatched
        ));
        for (status, now) in [(100, 1200), (180, 1300)] {
            assert!(matches!(
                retirement.receive(&response(&packet, status, "")?, packet.peer, now)?,
                RegistrationRetirementReceipt::Provisional(code) if code == status
            ));
        }
        assert!(matches!(
            retirement.poll(32_999)?,
            RegistrationRetirementStatus::Waiting
        ));
        assert_eq!(retirement.expires_at(), 33_000);
        assert!(matches!(
            retirement.receive(&valid, packet.peer, 32_999)?,
            RegistrationRetirementReceipt::Final(200)
        ));
    }
    Ok(())
}

#[test]
fn malformed_receipts_and_backward_clocks_preserve_both_waiter_and_original_request(
) -> Result<(), SipError> {
    for protocol in ["udp", "tcp", "tls"] {
        let (mut pending, packet, mut retirement) = prepared(protocol, 1000)?;
        let identity = pending.identity();
        let next = pending.next_deadline();
        let valid = response(&packet, 200, "")?;
        let text = std::str::from_utf8(&valid)?;
        let duplicate_call_id = response(
            &packet,
            200,
            &format!("Call-ID: {}\r\n", header(&packet, "Call-ID")?),
        )?;
        let duplicate_sequence = response(&packet, 200, "CSeq: 7 REGISTER\r\n")?;
        assert!(matches!(
            retirement.receive(&response(&packet, 100, "")?, packet.peer, 1100)?,
            RegistrationRetirementReceipt::Provisional(100)
        ));
        for malformed in [
            text.replace("Content-Length: 0", "Content-Length: 1"),
            text.replace("\r\n", "\n"),
            format!("{text}trailing"),
            text.replace(";branch=", ";branch=z9hG4bK-other;branch="),
            std::str::from_utf8(&duplicate_call_id)?.into(),
            std::str::from_utf8(&duplicate_sequence)?.into(),
        ] {
            assert!(matches!(
                retirement.receive(malformed.as_bytes(), packet.peer, 1100),
                Err(SipError::InvalidPacket)
            ));
        }
        assert!(matches!(
            retirement.receive(&b"x".repeat(MAX_MESSAGE + 1), packet.peer, 1100),
            Err(SipError::Capacity)
        ));
        assert!(matches!(
            retirement.receive(&valid, packet.peer, 1099),
            Err(SipError::InvalidPacket)
        ));
        assert!(matches!(
            retirement.poll(1099),
            Err(SipError::InvalidPacket)
        ));
        assert!(matches!(
            retirement.poll(1100)?,
            RegistrationRetirementStatus::Waiting
        ));
        assert_eq!(retirement.expires_at(), 33_000);
        assert_eq!(pending.identity(), identity);
        assert_eq!(pending.next_deadline(), next);
        assert_eq!(pending.expires_at(), 33_000);
        let original = final_response(pending.receive(&valid, packet.peer, 1100)?)?;
        assert_eq!(original.status(), 200);
        assert!(matches!(
            retirement.receive(&valid, packet.peer, 1100)?,
            RegistrationRetirementReceipt::Final(200)
        ));
    }
    Ok(())
}

#[test]
fn original_deadlines_release_exactly_once_without_extension_or_clock_revival(
) -> Result<(), SipError> {
    for protocol in ["udp", "tcp", "tls"] {
        for start in [1000, u64::MAX - 32_000] {
            let (mut pending, packet, mut retirement) = prepared(protocol, start)?;
            let deadline = pending.expires_at();
            pending.cancel();
            assert!(matches!(
                retirement.poll(deadline - 1)?,
                RegistrationRetirementStatus::Waiting
            ));
            assert!(matches!(
                retirement.poll(deadline)?,
                RegistrationRetirementStatus::Released
            ));
            assert!(matches!(
                retirement.receive(&response(&packet, 200, "")?, packet.peer, deadline)?,
                RegistrationRetirementReceipt::Unmatched
            ));
            for now in [u64::MAX, start, 0] {
                assert!(matches!(
                    retirement.poll(now)?,
                    RegistrationRetirementStatus::Released
                ));
            }
            assert_eq!(retirement.expires_at(), deadline);
        }
    }
    Ok(())
}

#[test]
fn retired_reliable_responses_preserve_the_existing_content_length_contract() -> Result<(), SipError>
{
    for protocol in ["udp", "tcp", "tls"] {
        let (_pending, packet, mut retirement) = prepared(protocol, 1000)?;
        let valid = response(&packet, 200, "")?;
        let omitted = std::str::from_utf8(&valid)?.replace("Content-Length: 0\r\n", "");
        if protocol == "udp" {
            assert!(matches!(
                retirement.receive(omitted.as_bytes(), packet.peer, 1100)?,
                RegistrationRetirementReceipt::Final(200)
            ));
        } else {
            assert!(matches!(
                retirement.receive(omitted.as_bytes(), packet.peer, 1100),
                Err(SipError::InvalidPacket)
            ));
            assert!(matches!(
                retirement.poll(1100)?,
                RegistrationRetirementStatus::Waiting
            ));
            assert!(matches!(
                retirement.receive(&valid, packet.peer, 1100)?,
                RegistrationRetirementReceipt::Final(200)
            ));
        }
    }
    Ok(())
}
