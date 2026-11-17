use super::{
    configuration, contact, first, register, registration, registration_authorization, routed,
    Engine, Protocol, SipError, StreamFlowId, PEER,
};
use rsipstack::sip::SipMessage;
use rusterisk_sip::AdmissionMode;

#[test]
fn quiescence_defers_registration_updates_without_consuming_authentication() -> Result<(), SipError>
{
    for protocol in [Protocol::Tcp, Protocol::Tls] {
        let mut engine = Engine::new(configuration(protocol)?, [71; 32]).with_transport(protocol);
        let flow = StreamFlowId::new(13)?;
        let previous = "127.0.0.1:5062".parse()?;
        let previous_contact = contact(protocol, "previous-contact")?;
        register(
            &mut engine,
            protocol,
            previous,
            StreamFlowId::new(14)?,
            "previous-publication",
            &[(previous_contact.as_str(), 60, 900)],
            0,
        )?;
        let uri = contact(protocol, "publication-contact")?;
        let contacts = [(uri.as_str(), 60, 1000)];
        let initial = registration(protocol, PEER, "publication", 1, &contacts, "")?;
        let challenge = engine.receive_on_flow(initial.as_bytes(), PEER, flow, 2)?;
        let authorization = registration_authorization(protocol, &first(&challenge)?.bytes)?;
        let authenticated =
            registration(protocol, PEER, "publication", 2, &contacts, &authorization)?;
        engine.set_admission(AdmissionMode::Quiesced);
        let replay = engine.receive_on_flow(initial.as_bytes(), PEER, flow, 3)?;
        assert_eq!(first(&replay)?.bytes, first(&challenge)?.bytes);
        let rejected = engine.receive_on_flow(authenticated.as_bytes(), PEER, flow, 4)?;
        routed(&rejected, PEER, flow);
        let SipMessage::Response(response) =
            SipMessage::try_from(first(&rejected)?.bytes.as_slice())?
        else {
            return Err(SipError::InvalidPacket);
        };
        assert_eq!(response.status_code.code(), 503);
        assert!(rejected.events.is_empty());
        assert_eq!(engine.registered_peer("phone", 4), Some(previous));
        let fresh = registration(protocol, PEER, "fresh-publication", 1, &contacts, "")?;
        let rejected = engine.receive_on_flow(fresh.as_bytes(), PEER, flow, 5)?;
        let SipMessage::Response(response) =
            SipMessage::try_from(first(&rejected)?.bytes.as_slice())?
        else {
            return Err(SipError::InvalidPacket);
        };
        assert_eq!(response.status_code.code(), 503);
        assert!(rejected.events.is_empty());
        assert_eq!(engine.registered_peer("phone", 5), Some(previous));
        engine.set_admission(AdmissionMode::Open);
        let admitted = engine.receive_on_flow(authenticated.as_bytes(), PEER, flow, 6)?;
        let SipMessage::Response(response) =
            SipMessage::try_from(first(&admitted)?.bytes.as_slice())?
        else {
            return Err(SipError::InvalidPacket);
        };
        assert_eq!(response.status_code.code(), 200);
        assert_eq!(engine.registered_peer("phone", 6), Some(PEER));
    }
    Ok(())
}
