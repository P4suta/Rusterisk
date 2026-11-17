use super::super::{config, invite, reauthenticate};
use super::{cancellation, incoming_id, incoming_request, response, response_tag};
use rsipstack::sip::{headers::typed::WwwAuthenticate, HeadersExt, SipMessage};
use rusterisk_config::Protocol;
use rusterisk_sip::{Engine, Output, SipError, SipEvent};
use std::net::SocketAddr;

struct Dialog {
    engine: Engine,
    protocol: Protocol,
    id: String,
    wire: String,
    peer: SocketAddr,
    local: SocketAddr,
}

fn on_transport(source: &str, protocol: Protocol) -> Result<String, SipError> {
    match protocol {
        Protocol::Udp => Ok(source.into()),
        Protocol::Tcp => Ok(source.replace("SIP/2.0/UDP", "SIP/2.0/TCP").replace(
            "Contact: <sip:phone@127.0.0.1:5061>",
            "Contact: <sip:phone@127.0.0.1:5061;transport=tcp>",
        )),
        Protocol::Tls | Protocol::Ws | Protocol::Wss => Err(SipError::Unsupported),
    }
}

fn dialog(protocol: Protocol) -> Result<Dialog, SipError> {
    let mut config = config()?;
    let transport = config
        .transports
        .first_mut()
        .ok_or(SipError::InvalidPacket)?;
    transport.name = "signaling".into();
    transport.protocol = protocol;
    for endpoint in &mut config.endpoints {
        endpoint.transport = "signaling".into();
    }
    let mut engine = Engine::new(config, [7; 32]).with_transport(protocol);
    let peer = "127.0.0.1:5061".parse()?;
    let challenged = engine.receive(on_transport(&invite("", 1), protocol)?.as_bytes(), peer, 0)?;
    let challenge = response(&challenged, 401)?;
    let parameters = WwwAuthenticate::parse(
        challenge
            .www_authenticate_header()
            .ok_or(SipError::InvalidPacket)?
            .value(),
    )?;
    let ack = incoming_request(&challenged.datagrams[0].bytes, "ACK", 1)?;
    let ack = std::str::from_utf8(&ack)?.replace("z9hG4bK-ACK-1", "z9hG4bK-1");
    engine.receive(on_transport(&ack, protocol)?.as_bytes(), peer, 0)?;
    let authorization = format!(
        "Authorization: Digest username=\"phone\", realm=\"{}\", nonce=\"{}\", uri=\"sip:100@localhost\", response=\"{}\", algorithm=MD5, qop=auth, cnonce=\"test-client\", nc=00000001\r\n",
        parameters.realm,
        parameters.nonce,
        "0".repeat(32),
    );
    let wire = reauthenticate(&on_transport(&invite(&authorization, 2), protocol)?, 1)?;
    let accepted = engine.receive(wire.as_bytes(), peer, 0)?;
    response(&accepted, 100)?;
    let id = incoming_id(&accepted)?;
    Ok(Dialog {
        engine,
        protocol,
        id,
        wire,
        peer,
        local: "127.0.0.1:10000".parse()?,
    })
}

fn exact(output: &Output, expected: &Output) {
    assert!(output.events.is_empty());
    assert_eq!(output.datagrams.len(), expected.datagrams.len());
    for (actual, expected) in output.datagrams.iter().zip(&expected.datagrams) {
        assert_eq!(actual.peer, expected.peer);
        assert_eq!(actual.origin(), expected.origin());
        assert_eq!(actual.bytes, expected.bytes);
    }
}

fn pending(dialog: &Dialog, tag: &Option<String>) -> Result<(), SipError> {
    let pending = dialog
        .engine
        .pending(&dialog.id)
        .ok_or(SipError::InvalidPacket)?;
    assert_eq!(&Some(pending.local_tag.clone()), tag);
    assert!(dialog.engine.active_session(&dialog.id, 9).is_none());
    Ok(())
}

fn acknowledge(
    dialog: &mut Dialog,
    output: &Output,
    now: u64,
    non_success: bool,
) -> Result<(), SipError> {
    let ack = incoming_request(&output.datagrams[0].bytes, "ACK", 2)?;
    let ack = std::str::from_utf8(&ack)?;
    let ack = if non_success {
        ack.replace("z9hG4bK-ACK-2", "z9hG4bK-2")
    } else {
        ack.to_owned()
    };
    let result = dialog.engine.receive(
        on_transport(&ack, dialog.protocol)?.as_bytes(),
        dialog.peer,
        now,
    )?;
    assert!(result.datagrams.is_empty());
    assert!(result.events.is_empty());
    Ok(())
}

#[test]
fn extended_plain_ringing_repeats_each_minute_and_stops_after_a_final_reply() -> Result<(), SipError>
{
    for protocol in [Protocol::Udp, Protocol::Tcp] {
        for cancel in [false, true] {
            let mut dialog = dialog(protocol)?;
            let ringing = dialog.engine.ring_at(&dialog.id, 0)?;
            let initial = response(&ringing, 180)?;
            let tag = response_tag(&ringing.datagrams[0].bytes)?;
            assert!(tag.is_some());
            assert!(initial.body.is_empty());
            exact(
                &dialog
                    .engine
                    .receive(dialog.wire.as_bytes(), dialog.peer, 0)?,
                &ringing,
            );
            for (now, due) in [
                (59_999, false),
                (60_000, true),
                (60_000, false),
                (119_999, false),
                (120_000, true),
            ] {
                let tick = dialog.engine.tick(now)?;
                if due {
                    exact(&tick, &ringing);
                } else {
                    assert!(tick.datagrams.is_empty());
                    assert!(tick.events.is_empty());
                }
                pending(&dialog, &tag)?;
            }
            if cancel {
                let request = cancellation(&dialog.wire)?;
                let ended = dialog
                    .engine
                    .receive(request.as_bytes(), dialog.peer, 120_001)?;
                assert_eq!(ended.datagrams.len(), 2);
                assert!(ended.datagrams[0].bytes.starts_with(b"SIP/2.0 200"));
                assert!(ended.datagrams[1].bytes.starts_with(b"SIP/2.0 487"));
                let terminated = Output {
                    datagrams: [ended.datagrams[1].clone()].into_iter().collect(),
                    events: Vec::new(),
                };
                acknowledge(&mut dialog, &terminated, 120_001, true)?;
            } else {
                let answer = dialog.engine.answer(&dialog.id, 9, dialog.local, 120_001)?;
                assert_eq!(response(&answer, 200)?.to_header()?, initial.to_header()?);
                acknowledge(&mut dialog, &answer, 120_001, false)?;
            }
            assert!(dialog.engine.pending(&dialog.id).is_none());
            assert_eq!(
                dialog.engine.active_session(&dialog.id, 9).is_none(),
                cancel
            );
            for now in [180_000, 240_000] {
                let tick = dialog.engine.tick(now)?;
                assert!(tick.datagrams.is_empty());
                assert!(tick.events.is_empty());
            }
        }
    }
    Ok(())
}

#[test]
fn ringing_uses_the_supplied_epoch_and_rejects_deadline_overflow_without_mutation(
) -> Result<(), SipError> {
    let epoch = 123_456;
    for protocol in [Protocol::Udp, Protocol::Tcp] {
        let mut dialog = dialog(protocol)?;
        let original = dialog
            .engine
            .receive(dialog.wire.as_bytes(), dialog.peer, 0)?;
        response(&original, 100)?;
        let tag = Some(
            dialog
                .engine
                .pending(&dialog.id)
                .ok_or(SipError::InvalidPacket)?
                .local_tag
                .clone(),
        );
        for now in [u64::MAX - 59_999, u64::MAX] {
            assert!(matches!(
                dialog.engine.ring_at(&dialog.id, now),
                Err(SipError::Capacity)
            ));
            pending(&dialog, &tag)?;
            exact(
                &dialog
                    .engine
                    .receive(dialog.wire.as_bytes(), dialog.peer, 0)?,
                &original,
            );
        }
        let ringing = dialog.engine.ring_at(&dialog.id, epoch)?;
        response(&ringing, 180)?;
        for (now, due) in [
            (epoch + 59_999, false),
            (epoch + 60_000, true),
            (epoch + 60_000, false),
            (epoch + 119_999, false),
            (epoch + 120_000, true),
        ] {
            let tick = dialog.engine.tick(now)?;
            if due {
                exact(&tick, &ringing);
            } else {
                assert!(tick.datagrams.is_empty());
                assert!(tick.events.is_empty());
            }
            pending(&dialog, &tag)?;
        }
    }
    Ok(())
}

#[test]
fn extended_pending_progress_repeats_each_minute_with_the_same_answer() -> Result<(), SipError> {
    for protocol in [Protocol::Udp, Protocol::Tcp] {
        let mut dialog = dialog(protocol)?;
        let progress = dialog.engine.progress(&dialog.id, 9, dialog.local, 0)?;
        let initial = response(&progress, 183)?;
        let tag = response_tag(&progress.datagrams[0].bytes)?;
        assert!(tag.is_some());
        for (now, due) in [
            (59_999, false),
            (60_000, true),
            (60_000, false),
            (119_999, false),
            (120_000, true),
            (120_000, false),
        ] {
            let tick = dialog.engine.tick(now)?;
            if due {
                exact(&tick, &progress);
            } else {
                assert!(tick.datagrams.is_empty());
                assert!(tick.events.is_empty());
            }
            pending(&dialog, &tag)?;
        }
        let answer = dialog.engine.answer(&dialog.id, 9, dialog.local, 120_001)?;
        let answered = response(&answer, 200)?;
        assert_eq!(answered.body, initial.body);
        assert_eq!(answered.to_header()?, initial.to_header()?);
        assert!(
            matches!(&answer.events[..], [SipEvent::Answered { id, generation: 9, .. }] if id == &dialog.id)
        );
    }
    Ok(())
}

#[test]
fn ringing_after_progress_replays_the_latest_reply_without_changing_the_final_answer(
) -> Result<(), SipError> {
    for protocol in [Protocol::Udp, Protocol::Tcp] {
        let mut dialog = dialog(protocol)?;
        let progress = dialog.engine.progress(&dialog.id, 9, dialog.local, 0)?;
        let prepared = response(&progress, 183)?;
        let latest = dialog.engine.ring(&dialog.id)?;
        assert_eq!(latest.datagrams.len(), 1);
        assert!(latest.events.is_empty());
        let SipMessage::Response(ringing) =
            SipMessage::try_from(latest.datagrams[0].bytes.as_slice())?
        else {
            return Err(SipError::InvalidPacket);
        };
        assert!((101..200).contains(&ringing.status_code.code()));
        assert_eq!(ringing.to_header()?, prepared.to_header()?);
        if !ringing.body.is_empty() {
            assert_eq!(ringing.body, prepared.body);
        }
        exact(
            &dialog
                .engine
                .receive(dialog.wire.as_bytes(), dialog.peer, 0)?,
            &latest,
        );
        assert!(dialog.engine.tick(59_999)?.datagrams.is_empty());
        exact(&dialog.engine.tick(60_000)?, &latest);
        let answer = dialog.engine.answer(&dialog.id, 9, dialog.local, 60_001)?;
        let answered = response(&answer, 200)?;
        assert_eq!(answered.body, prepared.body);
        assert_eq!(answered.to_header()?, prepared.to_header()?);
        assert_eq!(answered.contact_header()?, prepared.contact_header()?);
    }
    Ok(())
}

#[test]
fn a_final_answer_stops_the_pending_provisional_cadence() -> Result<(), SipError> {
    for protocol in [Protocol::Udp, Protocol::Tcp] {
        let mut dialog = dialog(protocol)?;
        dialog.engine.progress(&dialog.id, 9, dialog.local, 0)?;
        let answer = dialog.engine.answer(&dialog.id, 9, dialog.local, 59_999)?;
        acknowledge(&mut dialog, &answer, 59_999, false)?;
        for now in [60_000, 120_000] {
            let tick = dialog.engine.tick(now)?;
            assert!(tick.datagrams.is_empty());
            assert!(tick.events.is_empty());
            assert!(dialog.engine.pending(&dialog.id).is_none());
            assert!(dialog.engine.active_session(&dialog.id, 9).is_some());
        }
    }
    Ok(())
}

#[test]
fn cancellation_stops_the_pending_provisional_cadence() -> Result<(), SipError> {
    for protocol in [Protocol::Udp, Protocol::Tcp] {
        let mut dialog = dialog(protocol)?;
        dialog.engine.progress(&dialog.id, 9, dialog.local, 0)?;
        let cancel = cancellation(&dialog.wire)?;
        let ended = dialog
            .engine
            .receive(cancel.as_bytes(), dialog.peer, 59_999)?;
        assert_eq!(ended.datagrams.len(), 2);
        assert!(ended.datagrams[0].bytes.starts_with(b"SIP/2.0 200"));
        assert!(ended.datagrams[1].bytes.starts_with(b"SIP/2.0 487"));
        let terminated = Output {
            datagrams: [ended.datagrams[1].clone()].into_iter().collect(),
            events: Vec::new(),
        };
        acknowledge(&mut dialog, &terminated, 59_999, true)?;
        for now in [60_000, 120_000] {
            let tick = dialog.engine.tick(now)?;
            assert!(tick.datagrams.is_empty());
            assert!(tick.events.is_empty());
            assert!(dialog.engine.pending(&dialog.id).is_none());
            assert!(dialog.engine.active_session(&dialog.id, 9).is_none());
        }
    }
    Ok(())
}

#[test]
fn an_unrepresentable_first_provisional_deadline_does_not_publish_an_answer() -> Result<(), SipError>
{
    for protocol in [Protocol::Udp, Protocol::Tcp] {
        let mut dialog = dialog(protocol)?;
        let original = dialog
            .engine
            .receive(dialog.wire.as_bytes(), dialog.peer, 0)?;
        response(&original, 100)?;
        let before = dialog
            .engine
            .pending(&dialog.id)
            .ok_or(SipError::InvalidPacket)?;
        let tag = before.local_tag.clone();
        let request = before.request.clone();
        let session = before.session.clone();
        for now in [u64::MAX - 59_999, u64::MAX] {
            assert!(matches!(
                dialog.engine.progress(&dialog.id, 9, dialog.local, now),
                Err(SipError::Capacity)
            ));
            let after = dialog
                .engine
                .pending(&dialog.id)
                .ok_or(SipError::InvalidPacket)?;
            assert_eq!(after.local_tag, tag);
            assert_eq!(after.request, request);
            assert_eq!(after.session, session);
            exact(
                &dialog
                    .engine
                    .receive(dialog.wire.as_bytes(), dialog.peer, 0)?,
                &original,
            );
            assert!(dialog.engine.active_session(&dialog.id, 9).is_none());
        }
        let progress = dialog.engine.progress(&dialog.id, 9, dialog.local, 0)?;
        assert!(dialog.engine.tick(59_999)?.datagrams.is_empty());
        exact(&dialog.engine.tick(60_000)?, &progress);
    }
    Ok(())
}

#[test]
fn provisional_rescheduling_overflow_rejects_without_retiring_the_pending_answer(
) -> Result<(), SipError> {
    for protocol in [Protocol::Udp, Protocol::Tcp] {
        let mut dialog = dialog(protocol)?;
        let progress = dialog
            .engine
            .progress(&dialog.id, 9, dialog.local, u64::MAX - 60_000)?;
        let tag = response_tag(&progress.datagrams[0].bytes)?;
        assert!(dialog.engine.tick(u64::MAX - 1)?.datagrams.is_empty());
        for _ in 0..2 {
            assert!(matches!(
                dialog.engine.tick(u64::MAX),
                Err(SipError::Capacity)
            ));
            pending(&dialog, &tag)?;
            exact(
                &dialog
                    .engine
                    .receive(dialog.wire.as_bytes(), dialog.peer, u64::MAX)?,
                &progress,
            );
        }
    }
    Ok(())
}
