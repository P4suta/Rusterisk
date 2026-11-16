use rusterisk_config::{Config, Protocol};
use rusterisk_sip::{Engine, Outbound, SipError};

#[test]
fn websocket_contacts_preserve_sip_targets_and_the_registered_ws_parameter() -> Result<(), SipError>
{
    for (protocol, token) in [(Protocol::Ws, "ws"), (Protocol::Wss, "wss")] {
        let http = if protocol == Protocol::Wss {
            "[general]\nbindaddr=127.0.0.1\ntlsenable=yes\ntlsbindaddr=127.0.0.1:8089\ntlscertfile=certificate.pem\ntlsprivatekey=private-key.pem\n"
        } else {
            "[general]\nbindaddr=127.0.0.1\n"
        };
        let config = Config::parse(http, "[app]\ntype=user\npassword=test\n", &format!("[browser]\ntype=transport\nprotocol={token}\nbind=0.0.0.0\n[auth]\ntype=auth\nusername=phone\npassword=test\n[aor]\ntype=aor\ncontact=sip:phone@127.0.0.1:5061\n[phone]\ntype=endpoint\ntransport=browser\nauth=auth\naors=aor\nallow=ulaw\n"), "", "")?;
        let address = config.http.bind;
        let mut engine = Engine::new(config, [7; 32]).with_transport(protocol);
        let output = engine.originate(
            Outbound {
                id: "browser-call".into(),
                generation: 1,
                endpoint: "phone".into(),
                caller: "100".into(),
                local_rtp: "127.0.0.1:10000".parse()?,
                timeout_ms: None,
            },
            0,
        )?;
        let wire = std::str::from_utf8(&output.datagrams[0].bytes)?;
        assert!(wire.contains(&format!("Via: SIP/2.0/{} ", token.to_ascii_uppercase())));
        assert!(
            wire.contains(&format!(
                "Contact: <sip:rusterisk@{address};transport=ws>\r\n"
            )),
            "{wire}"
        );
    }
    Ok(())
}
