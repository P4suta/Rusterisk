use super::{answered, branch, config, forked, outbound, phase, request, transaction_response};
use rsipstack::sip::{headers::untyped::ToTypedHeader, HeadersExt, Method};
use rusterisk_sip::{Engine, SipError};

fn without_tag(response: &[u8], tag: &str) -> Result<Vec<u8>, SipError> {
    let source = std::str::from_utf8(response)?;
    let field = format!(";tag={tag}");
    assert_eq!(source.matches(&field).count(), 1);
    Ok(source.replace(&field, "").into_bytes())
}

#[test]
fn legacy_null_tags_remain_distinct_in_selected_and_unselected_dialogs() -> Result<(), SipError> {
    for selected_null in [false, true] {
        let mut settings = config()?;
        settings.aors[0].contact = Some("sip:phone@127.0.0.1:5061".into());
        let mut engine = Engine::new(settings, [9; 32]);
        let peer = "127.0.0.1:5061".parse()?;
        let invite = engine.originate(outbound(), 0)?.datagrams.remove(0).bytes;
        let winner = answered(&invite)?;
        let winner = if selected_null {
            without_tag(&winner, "callee")?
        } else {
            winner
        };
        let selected = phase("legacy selected answer", engine.receive(&winner, peer, 1))?;
        let [selected_ack] = selected.datagrams.as_slice() else {
            return Err(SipError::InvalidPacket);
        };
        assert!(engine.active_session("outgoing", 7).is_some());
        let response = forked(&invite, "other")?;
        let response = if selected_null {
            response
        } else {
            without_tag(&response, "other")?
        };
        let output = phase(
            "legacy unselected answer",
            engine.receive(&response, peer, 2),
        )?;
        assert!(output.events.is_empty());
        assert_eq!(
            output.datagrams.len(),
            2,
            "legacy unselected dialog must receive ACK and BYE"
        );
        let [ack, bye] = output.datagrams.as_slice() else {
            return Err(SipError::InvalidPacket);
        };
        for (packet, method, sequence) in [(ack, Method::Ack, 1), (bye, Method::Bye, 2)] {
            let message = request(&packet.bytes)?;
            assert_eq!(message.method, method);
            assert_eq!(message.cseq_header()?.typed()?.seq, sequence);
            assert_eq!(
                message.to_header()?.typed()?.tag().map(|tag| tag.value()),
                if selected_null { Some("other") } else { None }
            );
        }
        assert_ne!(branch(&ack.bytes)?, branch(&bye.bytes)?);
        assert_ne!(branch(&ack.bytes)?, branch(&selected_ack.bytes)?);
        phase(
            "legacy BYE settlement",
            engine.receive(&transaction_response(&bye.bytes, 200)?, peer, 3),
        )?;
        let repeated = phase(
            "legacy repeated unselected answer",
            engine.receive(&response, peer, 4),
        )?;
        let [repeated_ack] = repeated.datagrams.as_slice() else {
            return Err(SipError::InvalidPacket);
        };
        assert_eq!(repeated_ack.bytes, ack.bytes);
        assert!(repeated.events.is_empty());
        assert!(engine.active_session("outgoing", 7).is_some());
        let ended = phase(
            "legacy selected cancellation",
            engine.cancel("outgoing", 7, 5),
        )?;
        let [selected_bye] = ended.datagrams.as_slice() else {
            return Err(SipError::InvalidPacket);
        };
        assert_eq!(
            request(&selected_bye.bytes)?
                .to_header()?
                .typed()?
                .tag()
                .map(|tag| tag.value()),
            if selected_null { None } else { Some("callee") }
        );
    }
    Ok(())
}
