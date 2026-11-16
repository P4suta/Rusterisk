use md5::{Digest, Md5};
use rsipstack::sip::{HeadersExt, Response, SipMessage};
use rusterisk_config::Config;
use rusterisk_sip::{Engine, Output, SipError};
use sha2::{Sha256, Sha512_256};

#[path = "inbound_digest/body_integrity.rs"]
mod body_integrity;
#[path = "inbound_digest/challenge_capacity.rs"]
mod challenge_capacity;
#[path = "inbound_digest/integrity.rs"]
mod integrity;
#[path = "inbound_digest/nonce_binding.rs"]
mod nonce_binding;

fn engine(prefix: &str, algorithms: &str) -> Result<Engine, SipError> {
    configured(prefix, algorithms, "alice", "sample", "")
}

fn configured(
    prefix: &str,
    algorithms: &str,
    username: &str,
    password: &str,
    extra: &str,
) -> Result<Engine, SipError> {
    let config = Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=sample\n",
        &format!("{prefix}[udp]\ntype=transport\nprotocol=udp\nbind=127.0.0.1:5060\n[credential]\ntype=auth\nusername={username}\npassword={password}\nrealm=example.net\nsupported_algorithms_uas={algorithms}\n[line]\ntype=aor\nmax_contacts=1\n[phone]\ntype=endpoint\ntransport=udp\nauth=credential\naors=line\n{extra}"),
        "",
        "",
    )?;
    Ok(Engine::new(config, [7; 32]))
}

fn packet(sequence: u32, authorization: &str) -> String {
    format!("REGISTER sip:localhost SIP/2.0\r\nVia: SIP/2.0/UDP 127.0.0.1:5098;branch=z9hG4bK-inbound-{sequence};rport\r\nFrom: Displayed <sip:displayed@localhost>;tag=caller\r\nTo: <sip:line@localhost>\r\nCall-ID: inbound-digest\r\nCSeq: {sequence} REGISTER\r\nContact: <sip:contact@127.0.0.1:5098>\r\nExpires: 120\r\nMax-Forwards: 70\r\n{authorization}Content-Length: 0\r\n\r\n")
}

fn response(output: &Output) -> Result<Response, SipError> {
    let datagram = output.datagrams.first().ok_or(SipError::InvalidPacket)?;
    match SipMessage::try_from(datagram.bytes.as_slice())? {
        SipMessage::Response(response) => Ok(response),
        SipMessage::Request(_) => Err(SipError::InvalidPacket),
    }
}

fn attribute<'a>(source: &'a str, name: &str) -> Result<&'a str, SipError> {
    source
        .strip_prefix("Digest ")
        .ok_or(SipError::InvalidPacket)?
        .split(", ")
        .filter_map(|part| part.split_once('='))
        .find_map(|(key, value)| (key == name).then_some(value.trim_matches('"')))
        .ok_or(SipError::InvalidPacket)
}

fn md5(source: &str) -> String {
    Md5::digest(source.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[derive(Clone, Copy)]
enum Name<'a> {
    Plain,
    Hashed,
    Extended(&'a str),
}

fn hash(algorithm: &str, source: &str) -> Result<String, SipError> {
    match algorithm {
        "MD5" => Ok(md5(source)),
        "SHA-256" => Ok(Sha256::digest(source.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()),
        "SHA-512-256" => Ok(Sha512_256::digest(source.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()),
        _ => Err(SipError::Unsupported),
    }
}

fn authorization(
    offer: &str,
    username: &str,
    password: &str,
    name: Name<'_>,
) -> Result<String, SipError> {
    integrity::authorization(
        offer,
        username,
        password,
        name,
        "REGISTER",
        "sip:localhost",
        "",
    )
}

fn offer(response: &Response, algorithm: &str) -> Result<String, SipError> {
    response
        .headers
        .iter()
        .filter(|header| header.name().eq_ignore_ascii_case("WWW-Authenticate"))
        .find(|header| attribute(header.value(), "algorithm").is_ok_and(|value| value == algorithm))
        .map(|header| header.value().to_owned())
        .ok_or(SipError::InvalidPacket)
}

#[test]
fn inbound_challenges_use_endpoint_credentials_and_the_configured_algorithm_order(
) -> Result<(), SipError> {
    let mut engine = engine(
        "[remote]\ntype=auth\nusername=unused\npassword=sample\nrealm=provider.invalid\n",
        "SHA-256,SHA-512-256,MD5",
    )?;
    let response =
        response(&engine.receive(packet(1, "").as_bytes(), "127.0.0.1:5098".parse()?, 0)?)?;
    assert_eq!(response.status_code.code(), 401);
    let offers: Vec<_> = response
        .headers
        .iter()
        .filter(|header| header.name().eq_ignore_ascii_case("WWW-Authenticate"))
        .map(|header| header.value())
        .collect();
    let algorithms: Vec<_> = offers
        .iter()
        .map(|offer| attribute(offer, "algorithm"))
        .collect::<Result<_, _>>()?;
    assert_eq!(algorithms, ["SHA-256", "SHA-512-256", "MD5"]);
    for offer in offers {
        assert_eq!(attribute(offer, "realm")?, "example.net");
        assert_eq!(attribute(offer, "charset")?, "UTF-8");
        assert_eq!(attribute(offer, "userhash")?, "true");
    }
    Ok(())
}

#[test]
fn hashed_authentication_resolves_credentials_without_using_displayed_identity(
) -> Result<(), SipError> {
    let mut engine = engine("", "MD5")?;
    let peer = "127.0.0.1:5098".parse()?;
    let challenge = response(&engine.receive(packet(1, "").as_bytes(), peer, 0)?)?;
    let offer = challenge
        .www_authenticate_header()
        .ok_or(SipError::InvalidPacket)?
        .value();
    let nonce = attribute(offer, "nonce")?;
    let identity = md5("alice:example.net:sample");
    let target = md5("REGISTER:sip:localhost");
    let digest = md5(&format!(
        "{identity}:{nonce}:00000001:fixture-client:auth:{target}"
    ));
    let username = md5("alice:example.net");
    let authorization = format!("Authorization: Digest username=\"{username}\", realm=\"example.net\", nonce=\"{nonce}\", uri=\"sip:localhost\", response=\"{digest}\", algorithm=MD5, qop=auth, nc=00000001, cnonce=\"fixture-client\", userhash=true\r\n");
    let output = engine.receive(packet(2, &authorization).as_bytes(), peer, 1)?;
    assert_eq!(response(&output)?.status_code.code(), 200);
    assert_eq!(engine.registered_peer("phone", 2), Some(peer));
    Ok(())
}

#[test]
fn every_configured_algorithm_accepts_prepared_plain_hashed_and_extended_credentials(
) -> Result<(), SipError> {
    for algorithm in ["MD5", "SHA-256", "SHA-512-256"] {
        for name in [
            Name::Plain,
            Name::Hashed,
            Name::Extended("UTF-8''Al%C3%A9%20Smith"),
            Name::Extended("ISO-8859-1''Al%E9%20Smith"),
        ] {
            let mut engine = configured(
                "",
                algorithm,
                "\u{ff21}le\u{0301} Smith",
                "pa\u{030a}ss\u{00a0}word",
                "",
            )?;
            let peer = "127.0.0.1:5098".parse()?;
            let challenge = response(&engine.receive(packet(1, "").as_bytes(), peer, 0)?)?;
            let offer = offer(&challenge, algorithm)?;
            let signed = authorization(&offer, "Alé Smith", "påss word", name)?;
            let output = engine.receive(packet(2, &signed).as_bytes(), peer, 1)?;
            assert_eq!(response(&output)?.status_code.code(), 200, "{algorithm}");
            assert_eq!(engine.registered_peer("phone", 2), Some(peer));
            let replay = engine.receive(packet(3, &signed).as_bytes(), peer, 3)?;
            assert_eq!(response(&replay)?.status_code.code(), 401);
        }
    }
    Ok(())
}

#[test]
fn a_nonce_cannot_switch_algorithms_and_rejected_attempts_do_not_consume_its_counter(
) -> Result<(), SipError> {
    let mut engine = engine("", "SHA-256,SHA-512-256,MD5")?;
    let peer = "127.0.0.1:5098".parse()?;
    let challenge = response(&engine.receive(packet(1, "").as_bytes(), peer, 0)?)?;
    let offered = offer(&challenge, "SHA-256")?;
    let altered = offered.replace("algorithm=SHA-256", "algorithm=SHA-512-256");
    let altered = authorization(&altered, "alice", "sample", Name::Hashed)?;
    let rejected = engine.receive(packet(2, &altered).as_bytes(), peer, 1)?;
    assert_eq!(response(&rejected)?.status_code.code(), 401);
    let valid = authorization(&offered, "alice", "sample", Name::Hashed)?;
    let accepted = engine.receive(packet(3, &valid).as_bytes(), peer, 2)?;
    assert_eq!(response(&accepted)?.status_code.code(), 200);
    Ok(())
}

#[test]
fn distinct_auth_objects_with_the_same_prepared_identity_remain_ambiguous() -> Result<(), SipError>
{
    let extra = "[other]\ntype=auth\nusername=\u{ff41}lice\npassword=sample\nrealm=example.net\n[other]\ntype=endpoint\ntransport=udp\nauth=other\naors=line\ncontext=private\n";
    let mut engine = configured("", "MD5", "alice", "sample", extra)?;
    let peer = "127.0.0.1:5098".parse()?;
    let challenge = response(&engine.receive(packet(1, "").as_bytes(), peer, 0)?)?;
    let signed = authorization(&offer(&challenge, "MD5")?, "alice", "sample", Name::Hashed)?;
    let rejected = engine.receive(packet(2, &signed).as_bytes(), peer, 1)?;
    assert_eq!(response(&rejected)?.status_code.code(), 401);
    assert!(rejected.events.is_empty());
    assert_eq!(engine.registered_peer("phone", 2), None);
    Ok(())
}
