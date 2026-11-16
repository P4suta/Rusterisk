use super::{received, same_identity};
use crate::{
    authentication::{hash, header},
    transaction, transmitted,
};
use rusterisk_config::{Auth, Config};
use rusterisk_sip::{Datagram, RegistrationResponse, RegistrationTransaction, SipError};
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
        "[wildcard]\ntype=auth\nusername=account\npassword=pa\u{030a}ss\nrealm=*\n",
        "",
        "",
    )?
    .auth)
}

fn offer(scope: Scope, realm: &str, nonce: &str, utf8: bool, stale: bool) -> String {
    format!("{}: Digest realm=\"{realm}\", nonce=\"{nonce}\", algorithm=SHA-256, qop=\"auth-int\", opaque=\"{nonce}-opaque\", stale={stale}{}\r\n", scope.challenge(), if utf8 { ", charset=UTF-8" } else { "" })
}

fn authenticate(
    receipt: RegistrationResponse,
    credentials: &[Auth],
    client: &str,
    now: u64,
) -> Result<RegistrationTransaction, SipError> {
    let identity = receipt.identity().advance(u128::from(now))?;
    let next = receipt.authenticated_retry(credentials, identity, client, 1, now)?;
    assert!(
        next.is_some(),
        "The first challenge from the selected peer was rejected"
    );
    next.ok_or(SipError::Unsupported)
}

struct Expected<'a> {
    scope: Scope,
    realm: &'a str,
    nonce: &'a str,
    password: &'a str,
    client: &'a str,
    count: u32,
}

fn check(packet: &Datagram, expected: Expected<'_>) -> Result<(), SipError> {
    let authorization = header(packet, expected.scope.authorization())?;
    let uri = std::str::from_utf8(&packet.bytes)?
        .lines()
        .next()
        .and_then(|line| line.strip_prefix("REGISTER "))
        .and_then(|line| line.strip_suffix(" SIP/2.0"))
        .ok_or(SipError::InvalidPacket)?;
    let a1 = hash::<Sha256>(&format!("account:{}:{}", expected.realm, expected.password));
    let a2 = hash::<Sha256>(&format!("REGISTER:{uri}:{}", hash::<Sha256>("")));
    let response = hash::<Sha256>(&format!(
        "{a1}:{}:{:08x}:{}:auth-int:{a2}",
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
    assert!(authorization.contains("username=\"account\""));
    assert!(authorization.contains("algorithm=SHA-256, qop=auth-int"));
    assert!(!authorization.contains("charset="));
    Ok(())
}

#[test]
fn each_scope_accepts_a_fresh_realm_only_when_the_challenging_peer_changes() -> Result<(), SipError>
{
    let credentials = credentials()?;
    for scope in [Scope::Origin, Scope::Proxy] {
        let (receipt, _) = received(
            transaction("udp", 0)?,
            scope.status(),
            &offer(scope, "primary.example", "primary-nonce", false, false),
            0,
        )?;
        let pending = authenticate(receipt, &credentials, "primary-client", 200)?;
        let (receipt, original) = received(pending, 503, "Retry-After: 10\r\n", 200)?;
        let alternate = receipt.retry_alternate(
            &credentials,
            receipt.identity().advance(10)?,
            "127.0.0.3:5070".parse()?,
            400,
        )?;
        let (receipt, packet) = received(
            alternate,
            scope.status(),
            &offer(scope, "backup.example", "backup-nonce", false, false),
            400,
        )?;
        same_identity(&original, &packet)?;
        check(
            &packet,
            Expected {
                scope,
                realm: "primary.example",
                nonce: "primary-nonce",
                password: "pa\u{030a}ss",
                client: "primary-client",
                count: 2,
            },
        )?;
        let pending = authenticate(receipt, &credentials, "backup-client", 600)?;
        let (receipt, packet) = received(
            pending,
            scope.status(),
            &offer(
                scope,
                "backup.example",
                "changed-without-stale",
                false,
                false,
            ),
            600,
        )?;
        check(
            &packet,
            Expected {
                scope,
                realm: "backup.example",
                nonce: "backup-nonce",
                password: "pa\u{030a}ss",
                client: "backup-client",
                count: 1,
            },
        )?;
        let identity = receipt.identity().advance(11)?;
        assert!(receipt
            .authenticated_retry(&credentials, identity, "retry-client", 1, 800)?
            .is_none());
    }
    Ok(())
}

#[test]
fn peer_changes_keep_origin_proxy_encodings_and_counters_independent() -> Result<(), SipError> {
    let credentials = credentials()?;
    let (receipt, _) = received(
        transaction("udp", 0)?,
        401,
        &offer(
            Scope::Origin,
            "origin.primary",
            "origin-primary",
            true,
            false,
        ),
        0,
    )?;
    let pending = authenticate(receipt, &credentials, "origin-client", 200)?;
    let (receipt, _) = received(
        pending,
        407,
        &offer(Scope::Proxy, "proxy.primary", "proxy-primary", false, false),
        200,
    )?;
    let pending = authenticate(receipt, &credentials, "proxy-client", 400)?;
    let (receipt, _) = received(pending, 503, "Retry-After: 10\r\n", 400)?;
    let alternate = receipt.retry_alternate(
        &credentials,
        receipt.identity().advance(12)?,
        "127.0.0.3:5070".parse()?,
        600,
    )?;
    let (receipt, packet) = received(
        alternate,
        401,
        &offer(
            Scope::Origin,
            "origin.backup",
            "origin-backup",
            false,
            false,
        ),
        600,
    )?;
    check(
        &packet,
        Expected {
            scope: Scope::Origin,
            realm: "origin.primary",
            nonce: "origin-primary",
            password: "p\u{00e5}ss",
            client: "origin-client",
            count: 3,
        },
    )?;
    check(
        &packet,
        Expected {
            scope: Scope::Proxy,
            realm: "proxy.primary",
            nonce: "proxy-primary",
            password: "pa\u{030a}ss",
            client: "proxy-client",
            count: 2,
        },
    )?;
    let pending = authenticate(receipt, &credentials, "origin-backup-client", 800)?;
    let (receipt, packet) = received(
        pending,
        407,
        &offer(Scope::Proxy, "proxy.backup", "proxy-backup", true, false),
        800,
    )?;
    check(
        &packet,
        Expected {
            scope: Scope::Origin,
            realm: "origin.backup",
            nonce: "origin-backup",
            password: "pa\u{030a}ss",
            client: "origin-backup-client",
            count: 1,
        },
    )?;
    check(
        &packet,
        Expected {
            scope: Scope::Proxy,
            realm: "proxy.primary",
            nonce: "proxy-primary",
            password: "pa\u{030a}ss",
            client: "proxy-client",
            count: 3,
        },
    )?;
    let mut pending = authenticate(receipt, &credentials, "proxy-backup-client", 1000)?;
    let packet = transmitted(pending.poll(1000)?)?;
    check(
        &packet,
        Expected {
            scope: Scope::Origin,
            realm: "origin.backup",
            nonce: "origin-backup",
            password: "pa\u{030a}ss",
            client: "origin-backup-client",
            count: 2,
        },
    )?;
    check(
        &packet,
        Expected {
            scope: Scope::Proxy,
            realm: "proxy.backup",
            nonce: "proxy-backup",
            password: "p\u{00e5}ss",
            client: "proxy-backup-client",
            count: 1,
        },
    )?;
    Ok(())
}

#[test]
fn alternate_peers_start_a_new_four_challenge_budget_without_renewing_it_on_the_same_peer(
) -> Result<(), SipError> {
    let credentials = credentials()?;
    let mut pending = transaction("udp", 0)?;
    let mut now = 0;
    for attempt in 0..4 {
        let (receipt, _) = received(
            pending,
            401,
            &offer(
                Scope::Origin,
                "primary.example",
                &format!("primary-{attempt}"),
                false,
                true,
            ),
            now,
        )?;
        now += 200;
        pending = authenticate(receipt, &credentials, "primary-client", now)?;
    }
    let (receipt, _) = received(pending, 503, "Retry-After: 10\r\n", now)?;
    now += 200;
    pending = receipt.retry_alternate(
        &credentials,
        receipt.identity().advance(13)?,
        "127.0.0.3:5070".parse()?,
        now,
    )?;
    for attempt in 0..5 {
        let (receipt, _) = received(
            pending,
            401,
            &offer(
                Scope::Origin,
                "backup.example",
                &format!("backup-{attempt}"),
                false,
                attempt > 0,
            ),
            now,
        )?;
        now += 200;
        let identity = receipt.identity().advance(u128::from(now))?;
        let next = receipt.authenticated_retry(&credentials, identity, "backup-client", 1, now)?;
        if attempt == 4 {
            assert!(next.is_none());
            return Ok(());
        }
        assert!(
            next.is_some(),
            "The alternate peer did not receive its own challenge budget"
        );
        pending = next.ok_or(SipError::Unsupported)?;
    }
    Err(SipError::InvalidPacket)
}

#[test]
fn alternate_peer_challenges_cannot_escape_the_configured_credential_realm() -> Result<(), SipError>
{
    let mut credentials = credentials()?;
    for credential in &mut credentials {
        credential.realm = "primary.example".into();
    }
    let (receipt, _) = received(
        transaction("udp", 0)?,
        401,
        &offer(
            Scope::Origin,
            "primary.example",
            "primary-nonce",
            false,
            false,
        ),
        0,
    )?;
    let pending = authenticate(receipt, &credentials, "primary-client", 200)?;
    let (receipt, _) = received(pending, 503, "", 200)?;
    assert!(matches!(
        receipt.retry_alternate(
            &[],
            receipt.identity().advance(14)?,
            "127.0.0.3:5070".parse()?,
            400
        ),
        Err(SipError::Unsupported)
    ));
    let alternate = receipt.retry_alternate(
        &credentials,
        receipt.identity().advance(14)?,
        "127.0.0.3:5070".parse()?,
        400,
    )?;
    let (receipt, _) = received(
        alternate,
        401,
        &offer(
            Scope::Origin,
            "backup.example",
            "backup-nonce",
            false,
            false,
        ),
        400,
    )?;
    let identity = receipt.identity().advance(15)?;
    assert!(receipt
        .authenticated_retry(&credentials, identity, "backup-client", 1, 600)?
        .is_none());
    Ok(())
}

#[test]
fn alternate_requests_refuse_to_wrap_an_existing_nonce_counter() -> Result<(), SipError> {
    let credentials = credentials()?;
    let (receipt, _) = received(
        transaction("udp", 0)?,
        401,
        &offer(
            Scope::Origin,
            "primary.example",
            "primary-nonce",
            false,
            false,
        ),
        0,
    )?;
    let identity = receipt.identity().advance(16)?;
    let pending = receipt
        .authenticated_retry(&credentials, identity, "primary-client", u32::MAX, 200)?
        .ok_or(SipError::Unsupported)?;
    let (receipt, _) = received(pending, 503, "", 200)?;
    assert!(matches!(
        receipt.retry_alternate(
            &credentials,
            receipt.identity().advance(17)?,
            "127.0.0.3:5070".parse()?,
            400
        ),
        Err(SipError::Capacity)
    ));
    Ok(())
}
