use super::{answered, cleanup_requests, config, forked, outbound, request};
use crate::{authorization, challenge_invite, transaction_response};
use rsipstack::sip::{headers::untyped::ToTypedHeader, HeadersExt, Method};
use rusterisk_config::{Config, Protocol};
use rusterisk_sip::{Engine, SipError};
use std::{collections::BTreeSet, net::SocketAddr};
#[path = "boundaries/legacy.rs"]
mod legacy;
#[path = "boundaries/saturation.rs"]
mod saturation;

struct Call {
    engine: Engine,
    invite: Vec<u8>,
    peer: SocketAddr,
    ack: Vec<u8>,
}

fn phase<T>(name: &str, result: Result<T, SipError>) -> Result<T, SipError> {
    result.map_err(|error| SipError::Io(std::io::Error::other(format!("{name}: {error}"))))
}

fn settings(protocol: Protocol) -> Result<Config, SipError> {
    match protocol {
        Protocol::Udp => {
            let mut settings = config()?;
            settings.aors[0].contact = Some("sip:phone@127.0.0.1:5061".into());
            Ok(settings)
        }
        Protocol::Tls => Ok(Config::parse(
            "[general]\nbindaddr=127.0.0.1\n",
            "[app]\ntype=user\npassword=test\n",
            "[tls]\ntype=transport\nprotocol=tls\nbind=127.0.0.1:5060\ncert_file=certificate.pem\npriv_key_file=private.pem\n[a]\ntype=auth\nusername=phone\npassword=password\n[aor]\ntype=aor\ncontact=sips:phone@127.0.0.1:5061\\;transport=tcp\n[phone]\ntype=endpoint\ntransport=tls\nauth=a\naors=aor\nallow=ulaw\n",
            "",
            "",
        )?),
        Protocol::Tcp | Protocol::Ws | Protocol::Wss => Err(SipError::Unsupported),
    }
}

fn secure(bytes: &[u8]) -> Result<Vec<u8>, SipError> {
    Ok(std::str::from_utf8(bytes)?
        .replace(
            "Contact: <sip:phone@127.0.0.1:5061>",
            "Contact: <sips:phone@127.0.0.1:5061;transport=tcp>",
        )
        .into_bytes())
}

fn established(protocol: Protocol) -> Result<Call, SipError> {
    let mut engine = Engine::new(settings(protocol)?, [9; 32]).with_transport(protocol);
    let invite = engine.originate(outbound(), 0)?.datagrams.remove(0).bytes;
    let peer = "127.0.0.1:5061".parse()?;
    let response = answered(&invite)?;
    let response = if protocol == Protocol::Tls {
        secure(&response)?
    } else {
        response
    };
    let accepted = engine.receive(&response, peer, 1)?;
    assert!(engine.active_session("outgoing", 7).is_some());
    let [ack] = accepted.datagrams.as_slice() else {
        return Err(SipError::InvalidPacket);
    };
    Ok(Call {
        engine,
        invite,
        peer,
        ack: ack.bytes.clone(),
    })
}

fn branch(bytes: &[u8]) -> Result<String, SipError> {
    request(bytes)?
        .top_via_header()?
        .typed()?
        .branch()
        .map(ToString::to_string)
        .ok_or(SipError::InvalidPacket)
}

#[test]
fn each_fork_and_the_winner_keep_independent_sequences_and_transaction_branches(
) -> Result<(), SipError> {
    let mut call = established(Protocol::Udp)?;
    let mut branches = BTreeSet::new();
    assert!(branches.insert(branch(&call.ack)?));
    for (index, tag) in ["other-first", "other-second"].into_iter().enumerate() {
        let output =
            call.engine
                .receive(&forked(&call.invite, tag)?, call.peer, index as u64 + 2)?;
        assert!(output.events.is_empty());
        cleanup_requests(&output.datagrams, tag)?;
        for packet in output.datagrams {
            assert!(
                branches.insert(branch(&packet.bytes)?),
                "distinct dialogs reused a branch"
            );
        }
    }
    let ended = call.engine.cancel("outgoing", 7, 4)?;
    let [bye] = ended.datagrams.as_slice() else {
        return Err(SipError::InvalidPacket);
    };
    assert_eq!(request(&bye.bytes)?.cseq_header()?.typed()?.seq, 2);
    assert!(branches.insert(branch(&bye.bytes)?));
    assert_eq!(branches.len(), 6);
    Ok(())
}

#[test]
fn repeated_fork_answers_replay_the_ack_and_leave_bye_settlement_to_its_transaction(
) -> Result<(), SipError> {
    let mut call = established(Protocol::Udp)?;
    let response = forked(&call.invite, "other")?;
    let output = call.engine.receive(&response, call.peer, 2)?;
    cleanup_requests(&output.datagrams, "other")?;
    let [ack, bye] = output.datagrams.as_slice() else {
        return Err(SipError::InvalidPacket);
    };
    let repeated = call.engine.receive(&response, call.peer, 3)?;
    assert!(repeated.events.is_empty());
    let [repeated_ack] = repeated.datagrams.as_slice() else {
        return Err(SipError::InvalidPacket);
    };
    assert_eq!(repeated_ack.bytes, ack.bytes);
    let retry = call.engine.tick(502)?;
    let [repeated_bye] = retry.datagrams.as_slice() else {
        return Err(SipError::InvalidPacket);
    };
    assert_eq!(repeated_bye.bytes, bye.bytes);
    call.engine
        .receive(&transaction_response(&bye.bytes, 200)?, call.peer, 503)?;
    assert!(call.engine.tick(1502)?.datagrams.is_empty());
    let repeated = call.engine.receive(&response, call.peer, 1503)?;
    assert!(repeated.events.is_empty());
    let [repeated_ack] = repeated.datagrams.as_slice() else {
        return Err(SipError::InvalidPacket);
    };
    assert_eq!(repeated_ack.bytes, ack.bytes);
    assert!(call.engine.active_session("outgoing", 7).is_some());
    Ok(())
}

#[test]
fn a_changed_contact_cannot_redirect_the_ack_for_an_already_closed_fork() -> Result<(), SipError> {
    let mut call = established(Protocol::Udp)?;
    let response = forked(&call.invite, "other")?;
    let initial = call.engine.receive(&response, call.peer, 2)?;
    cleanup_requests(&initial.datagrams, "other")?;
    call.engine.receive(
        &transaction_response(&initial.datagrams[1].bytes, 200)?,
        call.peer,
        3,
    )?;
    let changed = std::str::from_utf8(&response)?.replace(
        "Contact: <sip:phone@127.0.0.1:5061>",
        "Contact: <sip:changed@127.0.0.1:5061>",
    );
    assert_ne!(changed.as_bytes(), response.as_slice());
    let repeated = call.engine.receive(changed.as_bytes(), call.peer, 4)?;
    assert!(repeated.events.is_empty());
    assert!(call.engine.active_session("outgoing", 7).is_some());
    let [ack] = repeated.datagrams.as_slice() else {
        return Err(SipError::InvalidPacket);
    };
    assert_eq!(request(&ack.bytes)?.method, Method::Ack);
    assert_eq!(
        request(&ack.bytes)?.uri,
        request(&initial.datagrams[0].bytes)?.uri,
        "A later Contact redirected the original fork acknowledgment",
    );
    assert!(
        ack.bytes == initial.datagrams[0].bytes,
        "A changed Contact changed the original fork acknowledgment"
    );
    let selected = call
        .engine
        .receive(&answered(&call.invite)?, call.peer, 5)?;
    let [selected_ack] = selected.datagrams.as_slice() else {
        return Err(SipError::InvalidPacket);
    };
    assert!(selected.events.is_empty());
    assert_eq!(selected_ack.bytes, call.ack);
    Ok(())
}

#[test]
fn fork_ack_retention_ends_with_the_first_response_window_without_extending_it(
) -> Result<(), SipError> {
    let mut call = established(Protocol::Udp)?;
    let first = call
        .engine
        .receive(&forked(&call.invite, "other-first")?, call.peer, 2)?;
    cleanup_requests(&first.datagrams, "other-first")?;
    let second_response = forked(&call.invite, "other-second")?;
    let second = call.engine.receive(&second_response, call.peer, 32_000)?;
    cleanup_requests(&second.datagrams, "other-second")?;
    let expired = call.engine.receive(&second_response, call.peer, 32_001)?;
    assert!(expired.datagrams.is_empty() && expired.events.is_empty());
    assert!(call.engine.active_session("outgoing", 7).is_some());
    Ok(())
}

#[test]
fn secure_forks_use_checked_targets_and_cannot_downgrade_the_winner() -> Result<(), SipError> {
    let mut call = established(Protocol::Tls)?;
    let invalid = forked(&call.invite, "insecure")?;
    assert!(matches!(
        call.engine.receive(&invalid, call.peer, 2),
        Err(SipError::Unsupported)
    ));
    assert!(call.engine.active_session("outgoing", 7).is_some());
    let valid = secure(&forked(&call.invite, "other")?)?;
    let output = call.engine.receive(&valid, call.peer, 3)?;
    cleanup_requests(&output.datagrams, "other")?;
    for packet in output.datagrams {
        assert!(std::str::from_utf8(&packet.bytes)?.contains(" SIP/2.0\r\nVia: SIP/2.0/TLS "));
        assert!(request(&packet.bytes)?.uri.to_string().starts_with("sips:"));
        assert!(!std::str::from_utf8(&packet.bytes)?.contains("transport=tls"));
    }
    let ended = call.engine.cancel("outgoing", 7, 4)?;
    assert_eq!(
        request(&ended.datagrams[0].bytes)?
            .to_header()?
            .typed()?
            .tag()
            .map(|tag| tag.value()),
        Some("callee")
    );
    assert!(request(&ended.datagrams[0].bytes)?
        .uri
        .to_string()
        .starts_with("sips:"));
    Ok(())
}

#[test]
fn fork_acknowledgments_preserve_both_invite_credentials_without_recomputing_them(
) -> Result<(), SipError> {
    let mut settings = phase("authenticated fork settings", settings(Protocol::Udp))?;
    settings.endpoints[0].outbound_auth.push("a".into());
    let mut engine = Engine::new(settings, [9; 32]);
    let peer = "127.0.0.1:5061".parse()?;
    let mut invite = phase(
        "initial authenticated fork INVITE",
        engine.originate(outbound(), 0),
    )?
    .datagrams
    .remove(0)
    .bytes;
    for (index, proxy) in [false, true].into_iter().enumerate() {
        let name = if proxy { "proxy" } else { "origin" };
        let challenge = phase(
            &format!("build {name} fork challenge"),
            challenge_invite(&invite, proxy),
        )?;
        let output = phase(
            &format!("receive {name} fork challenge"),
            engine.receive(&challenge, peer, index as u64 + 1),
        )?;
        assert!(output.events.is_empty());
        assert_eq!(
            output.datagrams.len(),
            2,
            "{name} challenge must produce ACK and authenticated INVITE"
        );
        let [ack, next] = output.datagrams.as_slice() else {
            return Err(SipError::InvalidPacket);
        };
        assert_eq!(request(&ack.bytes)?.method, Method::Ack);
        invite = next.bytes.clone();
    }
    let authenticated = phase("parse authenticated fork INVITE", request(&invite))?;
    assert_eq!(authenticated.cseq_header()?.typed()?.seq, 3);
    for proxy in [false, true] {
        assert!(authorization(&authenticated, proxy).is_some());
    }
    let winner = phase("build authenticated fork winner", answered(&invite))?;
    phase(
        "receive authenticated fork winner",
        engine.receive(&winner, peer, 3),
    )?;
    assert!(engine.active_session("outgoing", 7).is_some());
    let response = phase("build authenticated other fork", forked(&invite, "other"))?;
    let response = std::str::from_utf8(&response)?.replace(
        "Contact: <sip:phone@127.0.0.1:5061>",
        "Contact: <sip:branch@127.0.0.1:5061>",
    );
    let output = phase(
        "receive authenticated other fork",
        engine.receive(response.as_bytes(), peer, 4),
    )?;
    assert!(output.events.is_empty());
    assert_eq!(
        output.datagrams.len(),
        2,
        "authenticated other fork must produce ACK and BYE after setup completed"
    );
    let [ack, bye] = output.datagrams.as_slice() else {
        return Err(SipError::InvalidPacket);
    };
    let ack = request(&ack.bytes)?;
    let bye = request(&bye.bytes)?;
    assert_eq!(ack.method, Method::Ack);
    assert_eq!(bye.method, Method::Bye);
    assert_eq!(ack.cseq_header()?.typed()?.seq, 3);
    assert_eq!(bye.cseq_header()?.typed()?.seq, 4);
    assert_eq!(ack.uri.to_string(), "sip:branch@127.0.0.1:5061");
    for proxy in [false, true] {
        assert_eq!(
            authorization(&ack, proxy),
            authorization(&authenticated, proxy)
        );
    }
    Ok(())
}
