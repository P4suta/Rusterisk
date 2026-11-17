use super::{
    authenticate, dialog_request, engine, establish, first, incoming_id,
    invitation as parent_invitation, scoped, Protocol, SipError, SipEvent, StreamFlowId, PEER,
};
use rsipstack::sip::SipMessage;
use rusterisk_sip::{AdmissionMode, Output, PreparedEngineConfiguration};

fn invitation(
    protocol: Protocol,
    authorization: &str,
    sequence: u32,
    call: &str,
) -> Result<String, SipError> {
    Ok(parent_invitation(protocol, authorization, sequence, call)?
        .replace(";branch=z9hG4bK-", &format!(";branch=z9hG4bK-{call}-")))
}

fn status(output: &Output) -> Result<u16, SipError> {
    match SipMessage::try_from(first(output)?.bytes.as_slice())? {
        SipMessage::Response(response) => Ok(response.status_code.code()),
        SipMessage::Request(_) => Err(SipError::InvalidPacket),
    }
}

#[test]
fn quiescence_replays_existing_challenges_without_admitting_or_caching_new_calls(
) -> Result<(), SipError> {
    for protocol in [Protocol::Tcp, Protocol::Tls] {
        let mut engine = engine(protocol)?;
        let flow = StreamFlowId::new(10)?;
        let existing = invitation(protocol, "", 1, "existing-challenge")?;
        let challenge = engine.receive_on_flow(existing.as_bytes(), PEER, flow, 0)?;
        assert_eq!(status(&challenge)?, 401);
        engine.set_admission(AdmissionMode::Quiesced);
        let replay = engine.receive_on_flow(existing.as_bytes(), PEER, flow, 1)?;
        assert_eq!(first(&replay)?.bytes, first(&challenge)?.bytes);
        assert!(replay.events.is_empty());
        let initial = invitation(protocol, "", 1, "during-publication")?;
        let rejected = engine.receive_on_flow(initial.as_bytes(), PEER, flow, 2)?;
        scoped(&rejected, flow);
        assert_eq!(status(&rejected)?, 503);
        assert!(rejected.events.is_empty());
        engine.publish_configuration(PreparedEngineConfiguration::prepare(super::settings(
            protocol,
        )?)?);
        let unpublished = engine.receive_on_flow(initial.as_bytes(), PEER, flow, 3)?;
        assert_eq!(status(&unpublished)?, 503);
        assert!(unpublished.events.is_empty());
        engine.set_admission(AdmissionMode::Open);
        let resumed = engine.receive_on_flow(initial.as_bytes(), PEER, flow, 4)?;
        assert_eq!(status(&resumed)?, 401);
    }
    Ok(())
}

#[test]
fn quiescence_does_not_consume_an_authenticated_initial_request() -> Result<(), SipError> {
    for protocol in [Protocol::Tcp, Protocol::Tls] {
        let mut engine = engine(protocol)?;
        let flow = StreamFlowId::new(11)?;
        let challenge = engine.receive_on_flow(
            invitation(protocol, "", 1, "authenticated-publication")?.as_bytes(),
            PEER,
            flow,
            0,
        )?;
        let authorization = authenticate(&first(&challenge)?.bytes)?;
        let authenticated = invitation(protocol, &authorization, 2, "authenticated-publication")?;
        engine.set_admission(AdmissionMode::Quiesced);
        let rejected = engine.receive_on_flow(authenticated.as_bytes(), PEER, flow, 1)?;
        assert_eq!(status(&rejected)?, 503);
        assert!(rejected.events.is_empty());
        engine.set_admission(AdmissionMode::Open);
        let admitted = engine.receive_on_flow(authenticated.as_bytes(), PEER, flow, 2)?;
        assert!(engine.pending(&incoming_id(&admitted)?).is_some());
    }
    Ok(())
}

#[test]
fn quiescence_preserves_cached_dialog_replies_and_peer_termination() -> Result<(), SipError> {
    for protocol in [Protocol::Tcp, Protocol::Tls] {
        let mut engine = engine(protocol)?;
        let flow = StreamFlowId::new(12)?;
        let dialog = establish(&mut engine, protocol, flow, "established-publication", 0)?;
        engine.set_admission(AdmissionMode::Quiesced);
        let replay = engine.receive_on_flow(dialog.invitation.as_bytes(), PEER, flow, 4)?;
        assert_eq!(first(&replay)?.bytes, dialog.answer);
        assert!(replay.events.is_empty());
        let ended = engine.receive_on_flow(
            dialog_request(&dialog, protocol, "BYE", 3, "")?.as_bytes(),
            PEER,
            flow,
            5,
        )?;
        scoped(&ended, flow);
        assert_eq!(status(&ended)?, 200);
        assert!(ended
            .events
            .iter()
            .any(|event| matches!(event, SipEvent::Ended { id, .. } if id == &dialog.id)));
    }
    Ok(())
}

#[test]
fn legacy_receivers_share_the_quiescence_boundary() -> Result<(), SipError> {
    for protocol in [Protocol::Tcp, Protocol::Tls] {
        let mut engine = engine(protocol)?;
        let existing = invitation(protocol, "", 1, "legacy-existing")?;
        let challenge = engine.receive(existing.as_bytes(), PEER, 0)?;
        assert_eq!(status(&challenge)?, 401);
        engine.set_admission(AdmissionMode::Quiesced);
        let replay = engine.receive(existing.as_bytes(), PEER, 1)?;
        assert_eq!(first(&replay)?.bytes, first(&challenge)?.bytes);
        let initial = invitation(protocol, "", 1, "legacy-publication")?;
        let rejected = engine.receive(initial.as_bytes(), PEER, 2)?;
        assert_eq!(status(&rejected)?, 503);
        assert!(rejected.events.is_empty());
        assert!(rejected
            .datagrams
            .iter()
            .all(|packet| packet.flow().is_none()));
        engine.set_admission(AdmissionMode::Open);
        assert_eq!(status(&engine.receive(initial.as_bytes(), PEER, 3)?)?, 401);
    }
    Ok(())
}
