use rusterisk_config::{Config, Protocol};
use rusterisk_sip::{Engine, Outbound, SipError, SipEvent};

fn configuration(protocol: &str) -> Result<Config, SipError> {
    let files = if protocol == "tls" {
        "cert_file=certificate.pem\npriv_key_file=private-key.pem\n"
    } else {
        ""
    };
    Ok(Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=test\n",
        &format!(
            "[stream]\ntype=transport\nprotocol={protocol}\nbind=127.0.0.1:5060\n{files}[auth]\ntype=auth\nusername=phone\npassword=password\n[dynamic]\ntype=aor\ncontact=sip:phone@127.0.0.1:5061\n[phone]\ntype=endpoint\ntransport=stream\nauth=auth\naors=dynamic\nallow=ulaw\n"
        ),
        "",
        "",
    )?)
}

#[test]
fn reliable_invites_advertise_the_transport_and_timeout_without_retransmission(
) -> Result<(), SipError> {
    for (protocol, token) in [(Protocol::Tcp, "TCP"), (Protocol::Tls, "TLS")] {
        let mut engine = Engine::new(configuration(&token.to_ascii_lowercase())?, [7; 32])
            .with_transport(protocol);
        let output = engine.originate(
            Outbound {
                id: "stream-call".into(),
                generation: 1,
                endpoint: "phone".into(),
                caller: "100".into(),
                local_rtp: "127.0.0.1:10000".parse()?,
                timeout_ms: None,
            },
            0,
        )?;
        let wire = std::str::from_utf8(&output.datagrams[0].bytes)?;
        assert!(wire.contains(&format!("Via: SIP/2.0/{token} ")));
        assert!(wire.contains(";transport=tcp>\r\n"));
        for now in [500, 1500, 3500, 7500, 15_500, 31_500] {
            assert!(engine.tick(now)?.datagrams.is_empty());
        }
        let expired = engine.tick(32_000)?;
        assert!(expired.datagrams.is_empty());
        assert!(
            matches!(&expired.events[..], [SipEvent::Ended { id, generation: 1, .. }] if id == "stream-call")
        );
        assert!(engine.tick(32_001)?.events.is_empty());
    }
    Ok(())
}

#[test]
fn reliable_failed_invites_wait_for_ack_without_timer_g() -> Result<(), SipError> {
    let mut engine = Engine::new(configuration("tcp")?, [7; 32]).with_transport(Protocol::Tcp);
    let wire = "INVITE sip:100@localhost SIP/2.0\r\nVia: SIP/2.0/TCP 127.0.0.1:5061;branch=z9hG4bK-stream\r\nFrom: <sip:phone@localhost>;tag=phone\r\nTo: <sip:100@localhost>\r\nCall-ID: incoming-stream\r\nCSeq: 1 INVITE\r\nContact: <sip:phone@127.0.0.1:5061;transport=tcp>\r\nMax-Forwards: 70\r\nContent-Length: 0\r\n\r\n";
    let first = engine.receive(wire.as_bytes(), "127.0.0.1:5061".parse()?, 0)?;
    assert!(first.datagrams[0].bytes.starts_with(b"SIP/2.0 401"));
    for now in [500, 1500, 3500, 7500, 15_500, 31_500] {
        assert!(engine.tick(now)?.datagrams.is_empty());
    }
    let duplicate = engine.receive(wire.as_bytes(), "127.0.0.1:5061".parse()?, 10)?;
    assert_eq!(duplicate.datagrams[0].bytes, first.datagrams[0].bytes);
    Ok(())
}
