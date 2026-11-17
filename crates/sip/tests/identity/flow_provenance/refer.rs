use super::{dialog_request, engine, establish, first, request, scoped, PEER};
use rsipstack::sip::{HeadersExt, Method, SipMessage};
use rusterisk_config::Protocol;
use rusterisk_sip::{
    DatagramOrigin, DialogRefer, Output, ReferProgress, SipError, SipEvent, StreamFlowId,
};

fn refer(output: Output) -> Result<DialogRefer, SipError> {
    assert_eq!(output.events.len(), 1);
    let mut requested = output.events.into_iter().filter_map(|event| {
        if let SipEvent::BlindTransferRequested {
            generation: 7,
            refer,
            ..
        } = event
        {
            Some(refer)
        } else {
            None
        }
    });
    let refer = requested.next().ok_or(SipError::InvalidPacket)?;
    assert!(requested.next().is_none());
    Ok(refer)
}

fn notification(output: &Output) -> Result<&rusterisk_sip::Datagram, SipError> {
    for packet in &output.datagrams {
        match SipMessage::try_from(packet.bytes.as_slice())? {
            SipMessage::Request(request) => {
                if request.method != Method::Notify {
                    return Err(SipError::InvalidPacket);
                }
                return Ok(packet);
            }
            SipMessage::Response(_) => {}
        }
    }
    Err(SipError::InvalidPacket)
}

fn receipt(bytes: &[u8]) -> Result<Vec<u8>, SipError> {
    let request = request(bytes)?;
    Ok(format!("SIP/2.0 200 OK\r\nVia: {}\r\nFrom: {}\r\nTo: {}\r\nCall-ID: {}\r\nCSeq: {}\r\nContent-Length: 0\r\n\r\n", request.top_via_header()?.value(), request.from_header()?.value(), request.to_header()?.value(), request.call_id_header()?.value(), request.cseq_header()?.value()).into_bytes())
}

#[test]
fn refer_origin_and_cached_acceptance_keep_their_independent_flow() -> Result<(), SipError> {
    for protocol in [Protocol::Tcp, Protocol::Tls] {
        let mut engine = engine(protocol)?;
        let original = StreamFlowId::new(601)?;
        let another = StreamFlowId::new(602)?;
        let dialog = establish(&mut engine, protocol, original, "refer-origin", 0)?;
        let wire = dialog_request(
            &dialog,
            protocol,
            "REFER",
            3,
            "Refer-To: <sip:200@target.invalid>\r\n",
        )?;
        let foreign = engine.receive_on_flow(wire.as_bytes(), PEER, another, 4)?;
        scoped(&foreign, another);
        assert!(first(&foreign)?.bytes.starts_with(b"SIP/2.0 481 "));
        assert_eq!(first(&foreign)?.origin(), DatagramOrigin::Direct);
        assert!(foreign.events.is_empty());
        let requested = refer(engine.receive_on_flow(wire.as_bytes(), PEER, original, 5)?)?;
        assert_eq!(
            engine
                .refer_identity(requested.key())
                .ok_or(SipError::InvalidPacket)?
                .flow(),
            Some(original)
        );
        let accepted = engine.refer_progress(requested.key(), ReferProgress::Trying, 6)?;
        scoped(&accepted, original);
        assert_eq!(accepted.datagrams.len(), 2);
        assert!(accepted
            .datagrams
            .iter()
            .all(|packet| packet.origin() == DatagramOrigin::Refer(requested.key())));
        assert_eq!(
            request(&notification(&accepted)?.bytes)?.method,
            Method::Notify
        );
        let replayed = engine.receive_on_flow(wire.as_bytes(), PEER, original, 7)?;
        scoped(&replayed, original);
        assert_eq!(replayed.datagrams.len(), 1);
        assert!(first(&replayed)?.bytes.starts_with(b"SIP/2.0 202 "));
        assert_eq!(
            first(&replayed)?.origin(),
            DatagramOrigin::Refer(requested.key())
        );
        let foreign = engine.receive_on_flow(wire.as_bytes(), PEER, another, 8)?;
        scoped(&foreign, another);
        assert!(first(&foreign)?.bytes.starts_with(b"SIP/2.0 481 "));
        assert_eq!(first(&foreign)?.origin(), DatagramOrigin::Direct);
        assert!(foreign.events.is_empty());
    }
    Ok(())
}

#[test]
fn notification_receipts_and_disconnect_release_only_their_own_flow() -> Result<(), SipError> {
    for protocol in [Protocol::Tcp, Protocol::Tls] {
        let mut engine = engine(protocol)?;
        let original = StreamFlowId::new(701)?;
        let another = StreamFlowId::new(702)?;
        let first_dialog = establish(&mut engine, protocol, original, "refer-first", 0)?;
        let second_dialog = establish(&mut engine, protocol, another, "refer-second", 4)?;
        let first_wire = dialog_request(
            &first_dialog,
            protocol,
            "REFER",
            3,
            "Refer-To: <sip:200@target.invalid>\r\n",
        )?;
        let second_wire = dialog_request(
            &second_dialog,
            protocol,
            "REFER",
            3,
            "Refer-To: <sip:300@target.invalid>\r\n",
        )?;
        let first_refer =
            refer(engine.receive_on_flow(first_wire.as_bytes(), PEER, original, 8)?)?;
        let second_refer =
            refer(engine.receive_on_flow(second_wire.as_bytes(), PEER, another, 9)?)?;
        assert_ne!(first_refer.key(), second_refer.key());
        assert_eq!(
            engine
                .refer_identity(first_refer.key())
                .ok_or(SipError::InvalidPacket)?
                .flow(),
            Some(original)
        );
        assert_eq!(
            engine
                .refer_identity(second_refer.key())
                .ok_or(SipError::InvalidPacket)?
                .flow(),
            Some(another)
        );
        let accepted = engine.refer_progress(first_refer.key(), ReferProgress::Trying, 10)?;
        scoped(&accepted, original);
        engine.receive_on_flow(
            &receipt(&notification(&accepted)?.bytes)?,
            PEER,
            original,
            11,
        )?;
        let accepted_second =
            engine.refer_progress(second_refer.key(), ReferProgress::Trying, 12)?;
        scoped(&accepted_second, another);
        let completed = engine.refer_progress(first_refer.key(), ReferProgress::Answered, 13)?;
        scoped(&completed, original);
        let final_packet = notification(&completed)?;
        assert_eq!(
            final_packet.origin(),
            DatagramOrigin::Refer(first_refer.key())
        );
        let final_receipt = receipt(&final_packet.bytes)?;
        let foreign = engine.receive_on_flow(&final_receipt, PEER, another, 14)?;
        assert!(foreign.datagrams.is_empty());
        assert!(foreign.events.is_empty());
        assert!(engine.refer_identity(first_refer.key()).is_some());
        let settled = engine.receive_on_flow(&final_receipt, PEER, original, 15)?;
        assert!(
            matches!(settled.events.as_slice(), [SipEvent::ReferSubscriptionEnded { refer }] if *refer == first_refer.key())
        );
        assert!(engine.refer_identity(first_refer.key()).is_none());
        assert!(engine.refer_identity(second_refer.key()).is_some());
        let closed = engine.disconnected_flow(original);
        assert!(closed.datagrams.is_empty());
        assert!(!closed.events.iter().any(|event| matches!(event, SipEvent::ReferSubscriptionEnded { refer } if *refer == second_refer.key())));
        assert!(engine.refer_identity(second_refer.key()).is_some());
        let completed_second =
            engine.refer_progress(second_refer.key(), ReferProgress::Answered, 16)?;
        scoped(&completed_second, another);
        let final_packet = notification(&completed_second)?;
        assert_eq!(
            final_packet.origin(),
            DatagramOrigin::Refer(second_refer.key())
        );
        let settled = engine.receive_on_flow(&receipt(&final_packet.bytes)?, PEER, another, 17)?;
        assert!(
            matches!(settled.events.as_slice(), [SipEvent::ReferSubscriptionEnded { refer }] if *refer == second_refer.key())
        );
        assert!(engine.active_session(&second_dialog.id, 7).is_some());
    }
    Ok(())
}
