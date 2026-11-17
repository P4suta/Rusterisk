use super::{
    alternate::{received, same_identity},
    authentication::{hash, header},
    transaction, transmitted,
};
use rusterisk_config::{Auth, Config};
use rusterisk_sip::{
    Datagram, RegistrationIdentity, RegistrationResponse, RegistrationTransaction, SipError,
};
use sha2::Sha256;

#[derive(Clone, Copy)]
enum Scope {
    Origin,
    Proxy,
}

impl Scope {
    fn challenge(self) -> &'static str {
        match self {
            Self::Origin => "WWW-Authenticate",
            Self::Proxy => "Proxy-Authenticate",
        }
    }

    fn authorization(self) -> &'static str {
        match self {
            Self::Origin => "Authorization",
            Self::Proxy => "Proxy-Authorization",
        }
    }

    fn status(self) -> u16 {
        match self {
            Self::Origin => 401,
            Self::Proxy => 407,
        }
    }
}

fn credentials() -> Result<Vec<Auth>, SipError> {
    Ok(Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=sample\n",
        "[account]\ntype=auth\nusername=account\npassword=pa\u{030a}ss\nrealm=*\n",
        "",
        "",
    )?
    .auth)
}

fn offer(scope: Scope, realm: &str, nonce: &str, utf8: bool) -> String {
    format!("{}: Digest realm=\"{realm}\", nonce=\"{nonce}\", algorithm=SHA-256, qop=\"auth-int\", opaque=\"{nonce}-opaque\"{}\r\n", scope.challenge(), if utf8 { ", charset=UTF-8" } else { "" })
}

fn authenticate(
    receipt: RegistrationResponse,
    credentials: &[Auth],
    client: &str,
    now: u64,
) -> Result<RegistrationTransaction, SipError> {
    let identity = receipt.identity().advance(u128::from(now))?;
    receipt
        .authenticated_retry(credentials, identity, client, 1, now)?
        .ok_or(SipError::Unsupported)
}

struct Expected<'a> {
    scope: Scope,
    realm: &'a str,
    nonce: &'a str,
    client: &'a str,
    password: &'a str,
    count: u32,
}

fn check(packet: &Datagram, expected: Expected<'_>) -> Result<(), SipError> {
    let authorization = header(packet, expected.scope.authorization())?;
    let uri = std::str::from_utf8(&packet.bytes)?
        .split_once("\r\n")
        .and_then(|(line, _)| line.strip_prefix("REGISTER "))
        .and_then(|line| line.strip_suffix(" SIP/2.0"))
        .ok_or(SipError::InvalidPacket)?;
    let identity = hash::<Sha256>(&format!("account:{}:{}", expected.realm, expected.password));
    let target = hash::<Sha256>(&format!("REGISTER:{uri}:{}", hash::<Sha256>("")));
    let response = hash::<Sha256>(&format!(
        "{identity}:{}:{:08x}:{}:auth-int:{target}",
        expected.nonce, expected.count, expected.client
    ));
    for required in [
        format!("realm=\"{}\"", expected.realm),
        format!("nonce=\"{}\"", expected.nonce),
        format!("opaque=\"{}-opaque\"", expected.nonce),
        format!("uri=\"{uri}\""),
        format!("cnonce=\"{}\"", expected.client),
        format!("nc={:08x}", expected.count),
        format!("response=\"{response}\""),
    ] {
        assert!(authorization.contains(&required));
    }
    assert!(!authorization.contains("charset="));
    Ok(())
}

#[test]
fn resolution_after_a_completed_failure_preserves_wire_identity_for_same_and_other_peers(
) -> Result<(), SipError> {
    let (receipt, original) = received(transaction("udp", 0)?, 500, "Retry-After: 1\r\n", 0)?;
    for peer in [original.peer, "127.0.0.3:5070".parse()?] {
        let identity = receipt.identity().advance(99)?;
        let mut next = receipt.retry_after_resolution(&[], identity, peer, 1100)?;
        assert_eq!(next.identity(), identity);
        assert_eq!(next.expires_at(), 33_100);
        assert!(matches!(next.poll(1099), Err(SipError::InvalidPacket)));
        let packet = transmitted(next.poll(1100)?)?;
        same_identity(&original, &packet)?;
        assert_eq!(packet.peer, peer);
        assert_eq!(header(&packet, "CSeq")?, "8 REGISTER");
        assert_ne!(header(&packet, "Via")?, header(&original, "Via")?);
        assert!(next.poll(1599)?.is_none());
        let retransmission = transmitted(next.poll(1600)?)?;
        assert!(
            retransmission.bytes == packet.bytes,
            "A resolved registration retry changed its retransmission bytes"
        );
        assert_eq!(retransmission.peer, peer);
        next.cancel();
        assert!(next.poll(u64::MAX)?.is_none());
    }
    assert_eq!(receipt.status(), 500);
    assert_eq!(receipt.peer(), original.peer);
    assert_eq!(
        receipt.retry_after()?.map(|interval| interval.get()),
        Some(1)
    );
    Ok(())
}

#[test]
fn resolution_continuation_keeps_failure_protocol_identity_route_and_clock_guards(
) -> Result<(), SipError> {
    let peer = "127.0.0.3:5070".parse()?;
    for status in [200, 423] {
        let (receipt, _) = received(transaction("udp", 0)?, status, "", 0)?;
        assert!(matches!(
            receipt.retry_after_resolution(&[], receipt.identity().advance(99)?, peer, 1100),
            Err(SipError::Unsupported)
        ));
    }
    for protocol in ["tcp", "tls"] {
        let (receipt, _) = received(transaction(protocol, 0)?, 500, "", 0)?;
        assert!(matches!(
            receipt.retry_after_resolution(&[], receipt.identity().advance(99)?, peer, 1100),
            Err(SipError::Unsupported)
        ));
    }
    let (receipt, _) = received(transaction("udp", 0)?, 500, "Retry-After: 1\r\n", 0)?;
    assert!(matches!(
        receipt.retry_alternate(&[], receipt.identity().advance(99)?, peer, 1100),
        Err(SipError::Unsupported)
    ));
    for peer in [
        "0.0.0.0:5070",
        "224.0.0.1:5070",
        "127.0.0.3:0",
        "[::1]:5070",
    ] {
        assert!(matches!(
            receipt.retry_after_resolution(
                &[],
                receipt.identity().advance(99)?,
                peer.parse()?,
                1100
            ),
            Err(SipError::InvalidPacket)
        ));
    }
    for identity in [
        RegistrationIdentity::new(0x1234, 7, 99)?,
        RegistrationIdentity::new(0x1234, 6, 99)?,
        RegistrationIdentity::new(0x9999, 8, 99)?,
    ] {
        assert!(matches!(
            receipt.retry_after_resolution(&[], identity, peer, 1100),
            Err(SipError::InvalidPacket)
        ));
    }
    assert!(matches!(
        receipt.retry_after_resolution(&[], receipt.identity().advance(99)?, peer, 99),
        Err(SipError::InvalidPacket)
    ));
    assert!(matches!(
        receipt.retry_after_resolution(&[], receipt.identity().advance(99)?, peer, u64::MAX),
        Err(SipError::Capacity)
    ));
    Ok(())
}

#[test]
fn fresh_resolution_preserves_origin_proxy_encodings_and_nonce_counters() -> Result<(), SipError> {
    let credentials = credentials()?;
    let (receipt, _) = received(
        transaction("udp", 0)?,
        401,
        &offer(Scope::Origin, "origin.primary", "origin-nonce", true),
        0,
    )?;
    let pending = authenticate(receipt, &credentials, "origin-client", 200)?;
    let (receipt, _) = received(
        pending,
        407,
        &offer(Scope::Proxy, "proxy.primary", "proxy-nonce", false),
        200,
    )?;
    let pending = authenticate(receipt, &credentials, "proxy-client", 400)?;
    let (receipt, original) = received(pending, 500, "Retry-After: 2\r\n", 400)?;
    for peer in [original.peer, "127.0.0.3:5070".parse()?] {
        let mut next = receipt.retry_after_resolution(
            &credentials,
            receipt.identity().advance(99)?,
            peer,
            2500,
        )?;
        let packet = transmitted(next.poll(2500)?)?;
        same_identity(&original, &packet)?;
        assert_eq!(packet.peer, peer);
        assert_eq!(header(&packet, "CSeq")?, "10 REGISTER");
        check(
            &packet,
            Expected {
                scope: Scope::Origin,
                realm: "origin.primary",
                nonce: "origin-nonce",
                client: "origin-client",
                password: "p\u{00e5}ss",
                count: 3,
            },
        )?;
        check(
            &packet,
            Expected {
                scope: Scope::Proxy,
                realm: "proxy.primary",
                nonce: "proxy-nonce",
                client: "proxy-client",
                password: "pa\u{030a}ss",
                count: 2,
            },
        )?;
    }
    Ok(())
}

#[test]
fn resolved_retries_accept_a_new_scope_realm_only_from_a_different_peer() -> Result<(), SipError> {
    let credentials = credentials()?;
    for scope in [Scope::Origin, Scope::Proxy] {
        let (receipt, _) = received(
            transaction("udp", 0)?,
            scope.status(),
            &offer(scope, "primary.example", "primary-nonce", false),
            0,
        )?;
        let pending = authenticate(receipt, &credentials, "primary-client", 200)?;
        let (receipt, original) = received(pending, 500, "Retry-After: 1\r\n", 200)?;
        for peer in [original.peer, "127.0.0.3:5070".parse()?] {
            let next = receipt.retry_after_resolution(
                &credentials,
                receipt.identity().advance(99)?,
                peer,
                1300,
            )?;
            let (challenge, packet) = received(
                next,
                scope.status(),
                &offer(scope, "backup.example", "backup-nonce", false),
                1300,
            )?;
            same_identity(&original, &packet)?;
            let identity = challenge.identity().advance(100)?;
            let next =
                challenge.authenticated_retry(&credentials, identity, "backup-client", 1, 1500)?;
            assert_eq!(
                next.is_some(),
                peer != original.peer,
                "Fresh resolution changed the peer boundary of a Digest challenge"
            );
            if let Some(mut next) = next {
                let packet = transmitted(next.poll(1500)?)?;
                check(
                    &packet,
                    Expected {
                        scope,
                        realm: "backup.example",
                        nonce: "backup-nonce",
                        client: "backup-client",
                        password: "pa\u{030a}ss",
                        count: 1,
                    },
                )?;
            }
        }
    }
    Ok(())
}
