use super::{configured, integrity, offer, packet, response, Name};
use rsipstack::sip::HeadersExt;
use rusterisk_sip::{Engine, Output, SipError, SipEvent};

const URI: &str = "sip:100@localhost";
const BODY: &str = "v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=call\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\nm=audio 10002 RTP/AVP 0\r\na=sendrecv\r\n";

fn invitation(sequence: u32, authorization: &str, body: &str) -> String {
    packet(sequence, authorization)
        .replacen(
            "REGISTER sip:localhost SIP/2.0\r\n",
            &format!("INVITE {URI} SIP/2.0\r\n"),
            1,
        )
        .replace(
            &format!("CSeq: {sequence} REGISTER"),
            &format!("CSeq: {sequence} INVITE"),
        )
        .replace("To: <sip:line@localhost>", &format!("To: <{URI}>"))
        .replace("Expires: 120\r\n", "")
        .replace(
            "Content-Length: 0\r\n\r\n",
            &format!(
                "Content-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            ),
        )
}

fn acknowledge(
    engine: &mut Engine,
    output: &Output,
    sequence: u32,
    now: u64,
) -> Result<(), SipError> {
    let response = response(output)?;
    let ack = format!("ACK {URI} SIP/2.0\r\nVia: {}\r\nFrom: {}\r\nTo: {}\r\nCall-ID: {}\r\nCSeq: {sequence} ACK\r\nMax-Forwards: 70\r\nContent-Length: 0\r\n\r\n", response.top_via_header()?.value(), response.from_header()?.value(), response.to_header()?.value(), response.call_id_header()?.value());
    let result = engine.receive(ack.as_bytes(), "127.0.0.1:5098".parse()?, now)?;
    assert!(result.datagrams.is_empty());
    assert!(result.events.is_empty());
    Ok(())
}

#[test]
fn integrity_authentication_binds_nonempty_initial_invite_bodies_without_consuming_bad_counts(
) -> Result<(), SipError> {
    for algorithm in ["MD5", "SHA-256", "SHA-512-256"] {
        let mut engine = configured(
            "",
            algorithm,
            "alice",
            "sample",
            "context=internal\nallow=ulaw\n",
        )?;
        let peer = "127.0.0.1:5098".parse()?;
        let challenge = engine.receive(invitation(1, "", BODY).as_bytes(), peer, 0)?;
        assert_eq!(response(&challenge)?.status_code.code(), 401);
        assert!(challenge.events.is_empty());
        let offered = offer(&response(&challenge)?, algorithm)?;
        acknowledge(&mut engine, &challenge, 1, 0)?;
        let signed = integrity::authorization(
            &offered,
            "alice",
            "sample",
            Name::Plain,
            "INVITE",
            URI,
            BODY,
        )?;
        let altered = BODY.replace("s=call", "s=mall");
        let rejected = engine.receive(invitation(2, &signed, &altered).as_bytes(), peer, 1)?;
        assert_eq!(response(&rejected)?.status_code.code(), 401, "{algorithm}");
        assert!(rejected.events.is_empty());
        assert_eq!(engine.registered_peer("phone", 1), None);
        acknowledge(&mut engine, &rejected, 2, 1)?;
        let accepted = engine.receive(invitation(3, &signed, BODY).as_bytes(), peer, 2)?;
        assert_eq!(response(&accepted)?.status_code.code(), 100, "{algorithm}");
        let [SipEvent::Incoming {
            id,
            identity,
            caller,
            context,
            ..
        }] = accepted.events.as_slice()
        else {
            return Err(SipError::InvalidPacket);
        };
        assert_eq!(identity, "alice");
        assert_eq!(caller, "displayed");
        assert_eq!(context, "internal");
        assert!(engine.pending(id).is_some());
        let replay = engine.receive(invitation(4, &signed, BODY).as_bytes(), peer, 3)?;
        assert_eq!(response(&replay)?.status_code.code(), 401);
        assert!(replay.events.is_empty());
        assert!(engine.pending(id).is_some());
    }
    Ok(())
}
