use rusterisk_config::{Config, Registration};
use rusterisk_sip::{
    Datagram, RegistrationIdentity, RegistrationTransaction, RegistrationTransactionEvent, SipError,
};
#[path = "outbound_registration/authentication.rs"]
mod authentication;
#[path = "outbound_registration/failure_retry.rs"]
mod failure_retry;
#[path = "outbound_registration/grants.rs"]
mod grants;
#[path = "outbound_registration/minimum_expiration.rs"]
mod minimum_expiration;
#[path = "outbound_registration/responses.rs"]
mod responses;
#[path = "outbound_registration/retry_after.rs"]
mod retry_after;
#[path = "outbound_registration/updates.rs"]
mod updates;
#[path = "outbound_registration/utf8.rs"]
mod utf8;

fn settings(protocol: &str, extra: &str) -> Result<Registration, SipError> {
    let tls = if protocol == "tls" {
        "cert_file=certificate.pem\npriv_key_file=private.pem\n"
    } else {
        ""
    };
    let registrar = match protocol {
        "udp" => "SIP:Provider.Example:05060\\;transport=UDP",
        "tcp" => "sip:provider.example\\;transport=tcp",
        "tls" => "sips:provider.example\\;transport=tcp",
        _ => return Err(SipError::Unsupported),
    };
    let scheme = if protocol == "tls" { "sips" } else { "sip" };
    let config = Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=sample\n",
        &format!("[stream]\ntype=transport\nprotocol={protocol}\nbind=127.0.0.1:5060\n{tls}[credential]\ntype=auth\nusername=account\npassword=sample\n[provider]\ntype=registration\ntransport=stream\noutbound_auth=credential\nserver_uri={registrar}\nclient_uri={scheme}:%61ccount@Provider.Example\ncontact_user=line+1\n{extra}"),
        "",
        "",
    )?;
    config
        .registrations
        .into_iter()
        .next()
        .ok_or(SipError::InvalidPacket)
}

fn transaction(protocol: &str, now: u64) -> Result<RegistrationTransaction, SipError> {
    RegistrationTransaction::new(
        &settings(protocol, "")?,
        "127.0.0.1:5068".parse()?,
        "127.0.0.2:5070".parse()?,
        RegistrationIdentity::new(0x1234, 7, 0x5678)?,
        now,
    )
}

fn transmitted(event: Option<RegistrationTransactionEvent>) -> Result<Datagram, SipError> {
    match event {
        Some(RegistrationTransactionEvent::Transmit(packet)) => Ok(packet),
        Some(
            RegistrationTransactionEvent::TimedOut
            | RegistrationTransactionEvent::Provisional(_)
            | RegistrationTransactionEvent::Response(_),
        )
        | None => Err(SipError::InvalidPacket),
    }
}

#[test]
fn register_preserves_uri_roles_and_octets_with_an_empty_body() -> Result<(), SipError> {
    let mut pending = transaction("udp", 1000)?;
    assert_eq!(pending.next_deadline(), Some(1000));
    let packet = transmitted(pending.poll(1000)?)?;
    assert_eq!(packet.peer, "127.0.0.2:5070".parse()?);
    let wire = std::str::from_utf8(&packet.bytes)?;
    assert!(wire.starts_with("REGISTER SIP:Provider.Example:05060;transport=UDP SIP/2.0\r\n"));
    assert!(wire.contains("Via: SIP/2.0/UDP 127.0.0.1:5068;rport;branch=z9hG4bK"));
    assert!(wire.contains("To: <sip:%61ccount@Provider.Example>\r\n"));
    assert!(wire.contains("From: <sip:%61ccount@Provider.Example>;tag="));
    assert!(wire.contains("CSeq: 7 REGISTER\r\n"));
    assert!(wire.contains("Contact: <sip:line+1@127.0.0.1:5068;transport=udp>\r\n"));
    assert!(wire.contains("Max-Forwards: 70\r\n"));
    assert!(wire.contains("Expires: 3600\r\n"));
    assert!(wire.ends_with("Content-Length: 0\r\n\r\n"));
    assert!(matches!(
        rsipstack::sip::SipMessage::try_from(packet.bytes.as_slice())?,
        rsipstack::sip::SipMessage::Request(request) if request.method == rsipstack::sip::Method::Register
    ));
    Ok(())
}

#[test]
fn request_sequences_advance_without_changing_the_boot_call_id() -> Result<(), SipError> {
    let mut identity = RegistrationIdentity::new(0x1234, 7, 0x5678)?;
    let settings = settings("udp", "")?;
    let mut call_ids = Vec::new();
    let mut branches = Vec::new();
    for sequence in 7..10 {
        let mut pending = RegistrationTransaction::new(
            &settings,
            "127.0.0.1:5068".parse()?,
            "127.0.0.2:5070".parse()?,
            identity,
            0,
        )?;
        let packet = transmitted(pending.poll(0)?)?;
        let wire = std::str::from_utf8(&packet.bytes)?;
        call_ids.push(
            wire.lines()
                .find(|line| line.starts_with("Call-ID: "))
                .ok_or(SipError::InvalidPacket)?
                .to_owned(),
        );
        branches.push(
            wire.lines()
                .find(|line| line.starts_with("Via: "))
                .ok_or(SipError::InvalidPacket)?
                .to_owned(),
        );
        assert!(wire.contains(&format!("CSeq: {sequence} REGISTER\r\n")));
        identity = identity.advance(0x5678)?;
    }
    assert!(call_ids.windows(2).all(|pair| pair[0] == pair[1]));
    assert!(branches.windows(2).all(|pair| pair[0] != pair[1]));
    assert!(matches!(
        RegistrationIdentity::new(0, 1 << 31, 0),
        Err(SipError::Capacity)
    ));
    assert!(matches!(
        RegistrationIdentity::new(0, (1 << 31) - 1, 0)?.advance(1),
        Err(SipError::Capacity)
    ));
    Ok(())
}

#[test]
fn datagram_retransmission_uses_timer_e_until_timer_f_terminates() -> Result<(), SipError> {
    let mut pending = transaction("udp", 0)?;
    let initial = transmitted(pending.poll(0)?)?;
    for now in [
        500, 1500, 3500, 7500, 11_500, 15_500, 19_500, 23_500, 27_500, 31_500,
    ] {
        assert!(pending.poll(now - 1)?.is_none());
        let retry = transmitted(pending.poll(now)?)?;
        assert_eq!(retry.peer, initial.peer);
        assert_eq!(retry.bytes, initial.bytes);
        assert!(pending.poll(now)?.is_none());
    }
    assert_eq!(pending.next_deadline(), Some(32_000));
    assert!(matches!(
        pending.poll(32_000)?,
        Some(RegistrationTransactionEvent::TimedOut)
    ));
    assert!(pending.is_finished());
    assert_eq!(pending.next_deadline(), None);
    assert!(pending.poll(u64::MAX)?.is_none());
    Ok(())
}

#[test]
fn reliable_requests_have_a_finite_deadline_without_retransmission() -> Result<(), SipError> {
    for (protocol, token, scheme) in [("tcp", "TCP", "sip"), ("tls", "TLS", "sips")] {
        let mut pending = transaction(protocol, 0)?;
        let packet = transmitted(pending.poll(0)?)?;
        let wire = std::str::from_utf8(&packet.bytes)?;
        assert!(wire.contains(&format!("Via: SIP/2.0/{token} ")));
        assert!(wire.contains(&format!(
            "Contact: <{scheme}:line+1@127.0.0.1:5068;transport=tcp>\r\n"
        )));
        assert_eq!(pending.next_deadline(), Some(32_000));
        for now in [500, 1500, 3500, 7500, 31_999] {
            assert!(pending.poll(now)?.is_none());
        }
        assert!(matches!(
            pending.poll(32_000)?,
            Some(RegistrationTransactionEvent::TimedOut)
        ));
    }
    Ok(())
}

#[test]
fn cancellation_is_absorbing_before_and_after_initial_transmission() -> Result<(), SipError> {
    for sent in [false, true] {
        let mut pending = transaction("udp", 1000)?;
        if sent {
            transmitted(pending.poll(1000)?)?;
        }
        pending.cancel();
        assert!(pending.is_finished());
        assert_eq!(pending.next_deadline(), None);
        for now in [0, 1000, 1500, 33_000, u64::MAX] {
            assert!(pending.poll(now)?.is_none());
        }
    }
    Ok(())
}

#[test]
fn delayed_polling_never_replays_a_burst_or_sends_after_expiration() -> Result<(), SipError> {
    let mut pending = transaction("udp", 0)?;
    transmitted(pending.poll(0)?)?;
    transmitted(pending.poll(10_000)?)?;
    assert!(pending.poll(10_000)?.is_none());
    assert_eq!(pending.next_deadline(), Some(11_000));
    assert!(matches!(
        pending.poll(32_000)?,
        Some(RegistrationTransactionEvent::TimedOut)
    ));
    let mut unsent = transaction("udp", 0)?;
    assert!(matches!(
        unsent.poll(32_000)?,
        Some(RegistrationTransactionEvent::TimedOut)
    ));
    assert!(matches!(
        transaction("udp", u64::MAX - 31_999),
        Err(SipError::Capacity)
    ));
    Ok(())
}

#[test]
fn clock_errors_preserve_state_and_cancel_preserves_the_settlement_boundary() -> Result<(), SipError>
{
    let mut pending = transaction("udp", 1000)?;
    assert!(matches!(pending.poll(999), Err(SipError::InvalidPacket)));
    transmitted(pending.poll(1000)?)?;
    assert!(matches!(pending.poll(999), Err(SipError::InvalidPacket)));
    assert_eq!(pending.expires_at(), 33_000);
    let identity = pending.identity();
    pending.cancel();
    assert_eq!(pending.identity(), identity);
    assert_eq!(pending.expires_at(), 33_000);
    let mut edge = transaction("udp", u64::MAX - 32_000)?;
    transmitted(edge.poll(u64::MAX - 32_000)?)?;
    transmitted(edge.poll(u64::MAX - 1)?)?;
    assert_eq!(edge.next_deadline(), Some(u64::MAX));
    assert!(matches!(
        edge.poll(u64::MAX)?,
        Some(RegistrationTransactionEvent::TimedOut)
    ));
    Ok(())
}

#[test]
fn invalid_routes_and_mutated_contact_users_cannot_construct_a_request() -> Result<(), SipError> {
    let settings = settings("udp", "")?;
    for address in ["0.0.0.0:5060", "224.0.0.1:5060", "127.0.0.1:0"] {
        assert!(RegistrationTransaction::new(
            &settings,
            address.parse()?,
            "127.0.0.2:5070".parse()?,
            RegistrationIdentity::new(0, 1, 0)?,
            0,
        )
        .is_err());
        assert!(RegistrationTransaction::new(
            &settings,
            "127.0.0.1:5068".parse()?,
            address.parse()?,
            RegistrationIdentity::new(0, 1, 0)?,
            0,
        )
        .is_err());
    }
    for user in ["", "bad@host", "bad%zz", "bad\r\nRoute: value", "bad value"] {
        let mut changed = settings.clone();
        changed.contact_user = user.into();
        assert!(RegistrationTransaction::new(
            &changed,
            "127.0.0.1:5068".parse()?,
            "127.0.0.2:5070".parse()?,
            RegistrationIdentity::new(0, 1, 0)?,
            0,
        )
        .is_err());
    }
    Ok(())
}
