use rsipstack::sip::{HeadersExt, Request, SipMessage};
use rusterisk_config::Config;
use rusterisk_sip::{Engine, Outbound, Output, SipError, SipEvent};
use sha2::{Digest, Sha256, Sha512_256};

fn engine() -> Result<Engine, SipError> {
    let config = Config::parse("[general]\nbindaddr=127.0.0.1\n", "[app]\ntype=user\npassword=sample\n", "[udp]\ntype=transport\nprotocol=udp\nbind=127.0.0.1:5060\n[incoming]\ntype=auth\nusername=incoming\npassword=sample\n[remote]\ntype=auth\nusername=outbound\npassword=remote-password\nrealm=provider.example\n[phone]\ntype=aor\ncontact=sip:phone@127.0.0.1:5061\n[phone]\ntype=endpoint\ntransport=udp\nauth=incoming\noutbound_auth=remote\naors=phone\nallow=ulaw\n", "", "")?;
    Ok(Engine::new(config, [9; 32]))
}

fn originate(engine: &mut Engine) -> Result<Vec<u8>, SipError> {
    Ok(engine
        .originate(
            Outbound {
                id: "authenticated-call".into(),
                generation: 1,
                endpoint: "phone".into(),
                caller: "displayed-caller".into(),
                local_rtp: "127.0.0.1:10000".parse()?,
                timeout_ms: None,
            },
            0,
        )?
        .datagrams
        .remove(0)
        .bytes)
}

fn request(bytes: &[u8]) -> Result<Request, SipError> {
    match SipMessage::try_from(bytes)? {
        SipMessage::Request(request) => Ok(request),
        SipMessage::Response(_) => Err(SipError::InvalidPacket),
    }
}

fn challenge(invite: &[u8], proxy: bool, offers: &[&str]) -> Result<Vec<u8>, SipError> {
    let invite = request(invite)?;
    let (code, reason, name) = if proxy {
        (407, "Proxy Authentication Required", "Proxy-Authenticate")
    } else {
        (401, "Unauthorized", "WWW-Authenticate")
    };
    let offers = offers
        .iter()
        .map(|offer| format!("{name}: {offer}\r\n"))
        .collect::<String>();
    Ok(format!("SIP/2.0 {code} {reason}\r\nVia: {}\r\nFrom: {}\r\nTo: {};tag=remote-challenge\r\nCall-ID: {}\r\nCSeq: {}\r\n{offers}Content-Length: 0\r\n\r\n", invite.top_via_header()?.value(), invite.from_header()?.value(), invite.to_header()?.value(), invite.call_id_header()?.value(), invite.cseq_header()?.value()).into_bytes())
}

fn authorized(output: &Output) -> Result<Request, SipError> {
    assert_eq!(output.datagrams.len(), 2);
    assert!(output.datagrams[0].bytes.starts_with(b"ACK "));
    assert!(output.events.is_empty());
    request(&output.datagrams[1].bytes)
}

fn authorization(request: &Request, proxy: bool) -> Result<&str, SipError> {
    let name = if proxy {
        "Proxy-Authorization"
    } else {
        "Authorization"
    };
    request
        .headers
        .iter()
        .find(|header| header.name().eq_ignore_ascii_case(name))
        .map(|header| header.value())
        .ok_or(SipError::InvalidPacket)
}

fn attribute<'a>(header: &'a str, name: &str) -> Result<&'a str, SipError> {
    header
        .strip_prefix("Digest ")
        .into_iter()
        .flat_map(|value| value.split(','))
        .filter_map(|value| value.trim().split_once('='))
        .find(|(candidate, _)| *candidate == name)
        .map(|(_, value)| {
            value
                .strip_prefix('"')
                .and_then(|value| value.strip_suffix('"'))
                .unwrap_or(value)
        })
        .ok_or(SipError::InvalidPacket)
}

fn sha256(source: impl AsRef<[u8]>) -> String {
    Sha256::digest(source)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn sha512256(source: impl AsRef<[u8]>) -> String {
    Sha512_256::digest(source)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[test]
fn invite_challenges_select_the_first_supported_matching_offer_and_hash_the_actual_entity(
) -> Result<(), SipError> {
    for proxy in [false, true] {
        let mut engine = engine()?;
        let initial = originate(&mut engine)?;
        let offers = [
            "Basic realm=\"provider.example\"",
            "Digest realm=\"provider.example\", nonce=\"unsupported\", algorithm=SHA-512, qop=\"auth\"",
            "Digest realm=\"another-provider\", nonce=\"wrong-realm\", algorithm=SHA-256, qop=\"auth\"",
            "Digest realm=\"provider.example\", nonce=\"selected-nonce\", algorithm=SHA-512-256, qop=\"auth,auth-int\", opaque=\"roundtrip\"",
            "Digest realm=\"provider.example\", nonce=\"later-nonce\", algorithm=MD5, qop=\"auth\"",
        ];
        let output = engine.receive(
            &challenge(&initial, proxy, &offers)?,
            "127.0.0.1:5061".parse()?,
            1,
        )?;
        let retried = authorized(&output)?;
        let initial = request(&initial)?;
        assert_eq!(retried.body, initial.body);
        assert_eq!(
            retried.from_header()?.value(),
            initial.from_header()?.value()
        );
        assert_eq!(retried.cseq_header()?.value(), "2 INVITE");
        assert_ne!(
            retried.top_via_header()?.value(),
            initial.top_via_header()?.value()
        );
        let header = authorization(&retried, proxy)?;
        assert_eq!(attribute(header, "algorithm")?, "SHA-512-256");
        assert_eq!(attribute(header, "username")?, "outbound");
        assert_eq!(attribute(header, "qop")?, "auth-int");
        assert_eq!(attribute(header, "opaque")?, "roundtrip");
        assert_eq!(attribute(header, "nc")?, "00000001");
        assert_eq!(attribute(header, "uri")?, retried.uri.to_string());
        let identity = sha512256("outbound:provider.example:remote-password");
        let entity = sha512256(&retried.body);
        let target = sha512256(format!("INVITE:{}:{entity}", retried.uri));
        let expected = sha512256(format!(
            "{identity}:selected-nonce:00000001:{}:auth-int:{target}",
            attribute(header, "cnonce")?
        ));
        assert_eq!(attribute(header, "response")?, expected);
    }
    Ok(())
}

#[test]
fn an_omitted_quality_offer_still_emits_explicit_authentication_parameters() -> Result<(), SipError>
{
    let mut engine = engine()?;
    let initial = originate(&mut engine)?;
    let output = engine.receive(
        &challenge(
            &initial,
            false,
            &["Digest realm=\"provider.example\", nonce=\"default-quality\", algorithm=SHA-256"],
        )?,
        "127.0.0.1:5061".parse()?,
        1,
    )?;
    let retried = authorized(&output)?;
    let header = authorization(&retried, false)?;
    assert_eq!(attribute(header, "qop")?, "auth");
    assert_eq!(attribute(header, "nc")?, "00000001");
    let identity = sha256("outbound:provider.example:remote-password");
    let target = sha256(format!("INVITE:{}", retried.uri));
    let expected = sha256(format!(
        "{identity}:default-quality:00000001:{}:auth:{target}",
        attribute(header, "cnonce")?
    ));
    assert_eq!(attribute(header, "response")?, expected);
    Ok(())
}

#[test]
fn unsupported_algorithms_do_not_create_an_unoffered_md5_retry() -> Result<(), SipError> {
    let mut engine = engine()?;
    let initial = originate(&mut engine)?;
    let output = engine.receive(&challenge(&initial, false, &["Digest realm=\"provider.example\", nonce=\"unsupported\", algorithm=SHA-512, qop=\"auth\""] )?, "127.0.0.1:5061".parse()?, 1)?;
    assert_eq!(output.datagrams.len(), 1);
    assert!(output.datagrams[0].bytes.starts_with(b"ACK "));
    assert!(matches!(
        &output.events[..],
        [SipEvent::Ended { cause: 21, .. }]
    ));
    Ok(())
}

#[test]
fn the_server_priority_is_preserved_when_more_than_one_algorithm_is_supported(
) -> Result<(), SipError> {
    let mut engine = engine()?;
    let initial = originate(&mut engine)?;
    let offers = [
        "Digest realm=\"provider.example\", nonce=\"preferred\", algorithm=MD5, qop=\"auth\"",
        "Digest realm=\"provider.example\", nonce=\"later\", algorithm=SHA-512-256, qop=\"auth\"",
    ];
    let output = engine.receive(
        &challenge(&initial, false, &offers)?,
        "127.0.0.1:5061".parse()?,
        1,
    )?;
    let retried = authorized(&output)?;
    let header = authorization(&retried, false)?;
    assert_eq!(attribute(header, "algorithm")?, "MD5");
    assert_eq!(attribute(header, "nonce")?, "preferred");
    Ok(())
}

#[test]
fn a_stale_challenge_retries_only_with_a_changed_nonce_and_fresh_sequence() -> Result<(), SipError>
{
    for (stale, nonce, retries) in [
        (false, "next", false),
        (true, "initial", false),
        (true, "next", true),
    ] {
        let mut engine = engine()?;
        let initial = originate(&mut engine)?;
        let first = engine.receive(&challenge(&initial, false, &["Digest realm=\"provider.example\", nonce=\"initial\", algorithm=SHA-512-256, qop=\"auth\""] )?, "127.0.0.1:5061".parse()?, 1)?;
        let first = authorized(&first)?.to_string().into_bytes();
        let offer = format!("Digest realm=\"provider.example\", nonce=\"{nonce}\", algorithm=SHA-512-256, qop=\"auth\", stale={stale}");
        let second = engine.receive(
            &challenge(&first, false, &[&offer])?,
            "127.0.0.1:5061".parse()?,
            2,
        )?;
        if retries {
            let retried = authorized(&second)?;
            assert_eq!(retried.cseq_header()?.value(), "3 INVITE");
            assert_eq!(attribute(authorization(&retried, false)?, "nonce")?, "next");
        } else {
            assert_eq!(second.datagrams.len(), 1);
            assert!(matches!(
                &second.events[..],
                [SipEvent::Ended { cause: 21, .. }]
            ));
        }
    }
    Ok(())
}

#[test]
fn alternating_authentication_scopes_renew_the_existing_nonce_count_and_entity_digest(
) -> Result<(), SipError> {
    for proxy_first in [false, true] {
        let mut engine = engine()?;
        let initial = originate(&mut engine)?;
        let first = engine.receive(
            &challenge(&initial, proxy_first, &["Digest realm=\"provider.example\", nonce=\"first-nonce\", algorithm=SHA-512-256, qop=\"auth-int\", opaque=\"preserved\""] )?,
            "127.0.0.1:5061".parse()?,
            1,
        )?;
        let first_request = authorized(&first)?;
        let first_authorization = authorization(&first_request, proxy_first)?;
        let client_nonce = attribute(first_authorization, "cnonce")?;
        let second = engine.receive(
            &challenge(&first.datagrams[1].bytes, !proxy_first, &["Digest realm=\"provider.example\", nonce=\"second-nonce\", algorithm=SHA-256, qop=\"auth\""] )?,
            "127.0.0.1:5061".parse()?,
            2,
        )?;
        let retried = authorized(&second)?;
        assert_eq!(retried.cseq_header()?.value(), "3 INVITE");
        assert_eq!(retried.body, first_request.body);
        assert!(!retried.body.is_empty());
        let renewed = authorization(&retried, proxy_first)?;
        assert_eq!(attribute(renewed, "nonce")?, "first-nonce");
        assert_eq!(attribute(renewed, "cnonce")?, client_nonce);
        assert_eq!(attribute(renewed, "nc")?, "00000002");
        assert_eq!(attribute(renewed, "opaque")?, "preserved");
        let identity = sha512256("outbound:provider.example:remote-password");
        let entity = sha512256(&retried.body);
        let target = sha512256(format!("INVITE:{}:{entity}", retried.uri));
        let expected = sha512256(format!(
            "{identity}:first-nonce:00000002:{client_nonce}:auth-int:{target}"
        ));
        assert_eq!(attribute(renewed, "response")?, expected);
        let added = authorization(&retried, !proxy_first)?;
        assert_eq!(attribute(added, "nonce")?, "second-nonce");
        assert_eq!(attribute(added, "nc")?, "00000001");
    }
    Ok(())
}
