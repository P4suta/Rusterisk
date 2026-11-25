use rsipstack::sip::{
    headers::{
        auth::{Algorithm, AuthQop},
        typed::WwwAuthenticate,
    },
    services::DigestGenerator,
    HeadersExt, Method, Response, SipMessage, Uri,
};
use rusterisk_config::Config;
use rusterisk_core::ContactPresence;
use rusterisk_sip::{
    parse_registration_contacts, Engine, Outbound, Output, RegistrationContacts, SipError, SipEvent,
};
use std::net::SocketAddr;

#[path = "registration_bindings/contact_replacement.rs"]
mod contact_replacement;

struct Registrar {
    engine: Engine,
    serial: u32,
}

impl Registrar {
    fn new(static_contact: &str) -> Result<Self, SipError> {
        let config = Config::parse("[general]\nbindaddr=127.0.0.1\n", "[application]\ntype=user\npassword=sample\n", &format!("[udp]\ntype=transport\nprotocol=udp\nbind=127.0.0.1:5060\n[credential]\ntype=auth\nusername=device\npassword=sample\n[phone]\ntype=aor\nmax_contacts=2\nminimum_expiration=1\nmaximum_expiration=60\ndefault_expiration=30\n{static_contact}[backup]\ntype=aor\nmax_contacts=2\nminimum_expiration=1\nmaximum_expiration=60\ndefault_expiration=30\n[phone]\ntype=endpoint\ntransport=udp\nauth=credential\naors=phone,backup\n"), "", "")?;
        Ok(Self::from_config(config))
    }

    fn from_config(config: Config) -> Self {
        Self {
            engine: Engine::new(config, [7; 32]),
            serial: 0,
        }
    }

    fn many_addresses() -> Result<Self, SipError> {
        let addresses = (0..9)
            .map(|index| format!("address{index}"))
            .collect::<Vec<_>>();
        let sections = addresses
            .iter()
            .map(|address| format!("[{address}]\ntype=aor\nmax_contacts=16\n"))
            .collect::<String>();
        let config = Config::parse("[general]\nbindaddr=127.0.0.1\n", "[application]\ntype=user\npassword=sample\n", &format!("[udp]\ntype=transport\nprotocol=udp\nbind=127.0.0.1:5060\n[credential]\ntype=auth\nusername=device\npassword=sample\n{sections}[phone]\ntype=endpoint\ntransport=udp\nauth=credential\naors={}\n", addresses.join(",")), "", "")?;
        Ok(Self::from_config(config))
    }

    fn request_identity(
        &mut self,
        peer: SocketAddr,
        target: &str,
        headers: &str,
        identity: (&str, u32),
        now: u64,
    ) -> Result<Output, SipError> {
        self.serial = self.serial.checked_add(1).ok_or(SipError::Capacity)?;
        let serial = self.serial;
        let (call_id, sequence) = identity;
        let authenticated_sequence = sequence.checked_add(1).ok_or(SipError::Capacity)?;
        let uri = Uri::try_from("sip:localhost")?;
        let packet = |sequence: u32, authorization: &str| {
            format!("REGISTER {uri} SIP/2.0\r\nVia: SIP/2.0/UDP {peer};branch=z9hG4bK-set-{serial}-{sequence};rport\r\nFrom: <sip:displayed@localhost>;tag=device-{serial}\r\nTo: <sip:{target}@localhost>\r\nCall-ID: {call_id}\r\nCSeq: {sequence} REGISTER\r\n{headers}Max-Forwards: 70\r\n{authorization}Content-Length: 0\r\n\r\n")
        };
        let challenged = self
            .engine
            .receive(packet(sequence, "").as_bytes(), peer, now)?;
        let challenged = response(&challenged)?;
        assert_eq!(challenged.status_code.code(), 401);
        let challenge = WwwAuthenticate::parse(
            challenged
                .www_authenticate_header()
                .ok_or(SipError::InvalidPacket)?
                .value(),
        )?;
        let quality = AuthQop::Auth {
            cnonce: "set-client".into(),
            nc: 1,
        };
        let digest = DigestGenerator {
            username: "device",
            password: "sample",
            nonce: &challenge.nonce,
            uri: &uri,
            realm: &challenge.realm,
            method: &Method::Register,
            qop: Some(&quality),
            algorithm: Algorithm::Md5,
        }
        .compute();
        let authorization = format!("Authorization: Digest username=\"device\", realm=\"{}\", nonce=\"{}\", uri=\"{uri}\", response=\"{digest}\", algorithm=MD5, qop=auth, nc=00000001, cnonce=\"set-client\"\r\n", challenge.realm, challenge.nonce);
        self.engine.receive(
            packet(authenticated_sequence, &authorization).as_bytes(),
            peer,
            now + 1,
        )
    }

    fn request(
        &mut self,
        peer: SocketAddr,
        target: &str,
        contacts: &str,
        expires: Option<u32>,
        now: u64,
    ) -> Result<Output, SipError> {
        let next = self.serial.checked_add(1).ok_or(SipError::Capacity)?;
        let call_id = format!("binding-set-{next}");
        let expiration = expires
            .map(|value| format!("Expires: {value}\r\n"))
            .unwrap_or_default();
        self.request_identity(
            peer,
            target,
            &format!("{contacts}{expiration}"),
            (&call_id, 1),
            now,
        )
    }
}

fn response(output: &Output) -> Result<Response, SipError> {
    match SipMessage::try_from(output.datagrams[0].bytes.as_slice())? {
        SipMessage::Response(response) => Ok(response),
        SipMessage::Request(_) => Err(SipError::InvalidPacket),
    }
}

fn contacts(output: &Output) -> Result<Vec<(String, u32)>, SipError> {
    let response = response(output)?;
    assert_eq!(response.status_code.code(), 200);
    let parsed = parse_registration_contacts(
        response
            .headers
            .iter()
            .filter(|header| header.name().eq_ignore_ascii_case("Contact"))
            .map(|header| header.value()),
        None,
    )
    .map_err(|_| SipError::InvalidPacket)?;
    match parsed {
        RegistrationContacts::Query => Ok(Vec::new()),
        RegistrationContacts::Bindings(bindings) => bindings
            .as_slice()
            .iter()
            .map(|contact| {
                Ok((
                    contact.source_uri().into(),
                    contact.expires().ok_or(SipError::InvalidPacket)?,
                ))
            })
            .collect(),
        RegistrationContacts::RemoveAll => Err(SipError::InvalidPacket),
    }
}

fn presence(output: &Output, expected: ContactPresence) {
    assert!(
        matches!(&output.events[..], [SipEvent::EndpointContact { endpoint, presence, .. }] if endpoint == "phone" && *presence == expected)
    );
}

#[test]
fn repeated_headers_and_lists_return_every_binding_and_prefer_the_highest_quality(
) -> Result<(), SipError> {
    let peer = "127.0.0.1:5061".parse()?;
    let mut registrar = Registrar::new("")?;
    let output = registrar.request(peer, "phone", "Contact: <sip:first@one.invalid>;expires=10;q=0.25\r\nContact: <sip:second@two.invalid>;expires=20;q=1.0\r\n", Some(30), 0)?;
    presence(&output, ContactPresence::Present);
    assert_eq!(contacts(&output)?.len(), 2);
    let query = registrar.request("127.0.0.1:5062".parse()?, "phone", "", None, 1000)?;
    assert_eq!(contacts(&query)?.len(), 2);
    assert!(query.events.is_empty());
    let outgoing = registrar.engine.originate(
        Outbound {
            id: "best-binding".into(),
            generation: 1,
            endpoint: "phone".into(),
            caller: "100".into(),
            local_rtp: "127.0.0.1:10000".parse()?,
        },
        1002,
    )?;
    assert_eq!(outgoing.datagrams[0].peer, peer);
    let request = match SipMessage::try_from(outgoing.datagrams[0].bytes.as_slice())? {
        SipMessage::Request(request) => request,
        SipMessage::Response(_) => return Err(SipError::InvalidPacket),
    };
    assert_eq!(request.uri, Uri::try_from("sip:second@two.invalid")?);
    Ok(())
}

#[test]
fn capacity_failures_leave_the_binding_set_unchanged_and_mixed_updates_are_atomic(
) -> Result<(), SipError> {
    let peer = "127.0.0.1:5061".parse()?;
    let mut registrar = Registrar::new("")?;
    registrar.request(
        peer,
        "phone",
        "Contact: <sip:first@one.invalid>, <sip:second@two.invalid>\r\n",
        Some(30),
        0,
    )?;
    let refused = registrar.request(
        peer,
        "phone",
        "Contact: <sip:third@three.invalid>, <sip:fourth@four.invalid>\r\n",
        Some(30),
        10,
    )?;
    assert_eq!(response(&refused)?.status_code.code(), 403);
    assert!(refused.events.is_empty());
    let query = registrar.request(peer, "phone", "", None, 20)?;
    assert_eq!(
        contacts(&query)?
            .iter()
            .map(|(uri, _)| uri.as_str())
            .collect::<Vec<_>>(),
        ["sip:first@one.invalid", "sip:second@two.invalid"]
    );
    let changed = registrar.request(
        peer,
        "phone",
        "Contact: <sip:first@one.invalid>;expires=0, <sip:third@three.invalid>;expires=20\r\n",
        None,
        30,
    )?;
    assert_eq!(
        contacts(&changed)?
            .iter()
            .map(|(uri, _)| uri.as_str())
            .collect::<Vec<_>>(),
        ["sip:second@two.invalid", "sip:third@three.invalid"]
    );
    assert!(changed.events.is_empty());
    Ok(())
}

#[test]
fn individual_expiry_removal_and_disconnect_preserve_other_owned_bindings() -> Result<(), SipError>
{
    let first = "127.0.0.1:5061".parse()?;
    let second = "127.0.0.1:5062".parse()?;
    let mut registrar = Registrar::new("")?;
    registrar.request(
        first,
        "phone",
        "Contact: <sip:first@one.invalid>;expires=2\r\n",
        None,
        0,
    )?;
    let refreshed = registrar.request(
        second,
        "phone",
        "Contact: <sip:second@two.invalid>;expires=4\r\n",
        None,
        10,
    )?;
    assert!(refreshed.events.is_empty());
    assert!(registrar.engine.tick(2001)?.events.is_empty());
    assert_eq!(
        registrar.engine.registered_peer("phone", 2001),
        Some(second)
    );
    assert!(registrar.engine.disconnected(first).events.is_empty());
    presence(
        &registrar.engine.disconnected(second),
        ContactPresence::Absent,
    );
    assert!(registrar.engine.disconnected(second).events.is_empty());
    assert!(registrar.engine.tick(5000)?.events.is_empty());
    registrar.request(
        first,
        "phone",
        "Contact: <sip:first@one.invalid>;expires=10\r\n",
        None,
        6000,
    )?;
    registrar.request(
        second,
        "phone",
        "Contact: <sip:second@two.invalid>;expires=10\r\n",
        None,
        6010,
    )?;
    assert!(registrar.engine.disconnected(second).events.is_empty());
    assert_eq!(registrar.engine.registered_peer("phone", 6020), Some(first));
    presence(
        &registrar.engine.disconnected(first),
        ContactPresence::Absent,
    );
    Ok(())
}

#[test]
fn wildcard_and_targeted_removal_observe_endpoint_presence_across_addresses() -> Result<(), SipError>
{
    let first = "127.0.0.1:5061".parse()?;
    let second = "127.0.0.1:5062".parse()?;
    let mut registrar = Registrar::new("")?;
    registrar.request(
        first,
        "phone",
        "Contact: <sip:first@one.invalid>\r\n",
        Some(30),
        0,
    )?;
    let added = registrar.request(
        second,
        "backup",
        "Contact: <sip:backup@two.invalid>\r\n",
        Some(30),
        10,
    )?;
    assert!(added.events.is_empty());
    let removed = registrar.request(
        first,
        "phone",
        "Contact: <sip:first@one.invalid>\r\n",
        Some(0),
        20,
    )?;
    assert!(contacts(&removed)?.is_empty());
    assert!(removed.events.is_empty());
    assert_eq!(registrar.engine.registered_peer("phone", 22), Some(second));
    let removed = registrar.request(second, "backup", "Contact: *\r\n", Some(0), 30)?;
    presence(&removed, ContactPresence::Absent);
    assert!(contacts(&removed)?.is_empty());
    assert_eq!(registrar.engine.registered_peer("phone", 32), None);
    Ok(())
}

#[test]
fn expiration_selection_uses_contact_then_header_then_the_address_policy() -> Result<(), SipError> {
    let peer = "127.0.0.1:5061".parse()?;
    let mut registrar = Registrar::new("")?;
    let output = registrar.request(
        peer,
        "phone",
        "Contact: <sip:first@one.invalid>;expires=5, <sip:second@two.invalid>\r\n",
        Some(120),
        0,
    )?;
    assert_eq!(
        contacts(&output)?,
        [
            ("sip:first@one.invalid".into(), 5),
            ("sip:second@two.invalid".into(), 60)
        ]
    );
    let cleared = registrar.request(peer, "phone", "Contact: *\r\n", Some(0), 10)?;
    assert!(contacts(&cleared)?.is_empty());
    let output = registrar.request(
        peer,
        "phone",
        "Contact: <sip:default@one.invalid>\r\n",
        None,
        20,
    )?;
    assert_eq!(contacts(&output)?[0].1, 30);
    Ok(())
}

#[test]
fn stale_sequences_reject_refreshes_and_wildcard_removal_without_partial_changes(
) -> Result<(), SipError> {
    let peer = "127.0.0.1:5061".parse()?;
    let mut registrar = Registrar::new("")?;
    registrar.request_identity(
        peer,
        "phone",
        "Contact: <sip:first@one.invalid>\r\n",
        ("stable", 10),
        0,
    )?;
    let stale = registrar.request_identity(
        peer,
        "phone",
        "Contact: <sip:first@one.invalid>;expires=0, <sip:second@two.invalid>\r\n",
        ("stable", 8),
        10,
    )?;
    assert_eq!(response(&stale)?.status_code.code(), 500);
    let stale = registrar.request_identity(
        peer,
        "phone",
        "Contact: *\r\nExpires: 0\r\n",
        ("stable", 10),
        20,
    )?;
    assert_eq!(response(&stale)?.status_code.code(), 500);
    let query = registrar.request(peer, "phone", "", None, 30)?;
    assert_eq!(contacts(&query)?[0].0, "sip:first@one.invalid");
    let changed = registrar.request_identity(
        peer,
        "phone",
        "Contact: <sip:first@one.invalid>;expires=0, <sip:second@two.invalid>\r\n",
        ("stable", 12),
        40,
    )?;
    assert_eq!(contacts(&changed)?[0].0, "sip:second@two.invalid");
    assert!(changed.events.is_empty());
    Ok(())
}

#[test]
fn ambiguous_identity_duplicates_and_insecure_contacts_leave_existing_bindings_unchanged(
) -> Result<(), SipError> {
    let peer = "127.0.0.1:5061".parse()?;
    let mut registrar = Registrar::new("")?;
    registrar.request(
        peer,
        "phone",
        "Contact: <sip:first@one.invalid;security=on>, <sip:first@one.invalid;security=off>\r\n",
        None,
        0,
    )?;
    for (headers, expected) in [
        ("Contact: <sip:first@one.invalid>;expires=0\r\n", 400),
        ("Contact: <sip:first@one.invalid;security=on>;expires=0, <sip:new@new.invalid>, <sip:%6Eew@NEW.invalid>\r\n", 400),
        ("Contact: <sip:first@one.invalid;security=on>;expires=0, <sips:secure@new.invalid>\r\n", 400),
    ] {
        let refused = registrar.request(peer, "phone", headers, None, 10)?;
        assert_eq!(response(&refused)?.status_code.code(), expected);
        assert!(refused.events.is_empty());
        let query = registrar.request(peer, "phone", "", None, 20)?;
        assert_eq!(contacts(&query)?.len(), 2);
    }
    Ok(())
}

#[test]
fn the_engine_capacity_is_shared_across_addresses_and_refusals_preserve_every_binding(
) -> Result<(), SipError> {
    let peer = "127.0.0.1:5061".parse()?;
    let mut registrar = Registrar::many_addresses()?;
    let headers = format!(
        "Contact: {}\r\n",
        (0..16)
            .map(|index| format!("<sip:device{index}@example.net>"))
            .collect::<Vec<_>>()
            .join(",")
    );
    for index in 0..8 {
        let output =
            registrar.request(peer, &format!("address{index}"), &headers, None, index * 10)?;
        assert_eq!(contacts(&output)?.len(), 16);
    }
    let refused = registrar.request(
        peer,
        "address8",
        "Contact: <sip:overflow@example.net>\r\n",
        None,
        100,
    )?;
    assert_eq!(response(&refused)?.status_code.code(), 503);
    assert!(refused.events.is_empty());
    for index in 0..8 {
        let output =
            registrar.request(peer, &format!("address{index}"), "", None, 110 + index * 10)?;
        assert_eq!(contacts(&output)?.len(), 16);
    }
    let output = registrar.request(peer, "address8", "", None, 200)?;
    assert!(contacts(&output)?.is_empty());
    Ok(())
}

#[test]
fn oversized_complete_responses_refuse_an_update_before_publishing_its_binding(
) -> Result<(), SipError> {
    let peer = "127.0.0.1:5061".parse()?;
    let mut registrar = Registrar::many_addresses()?;
    let mut accepted = 0;
    for index in 0..16 {
        let header = format!(
            "Contact: <sip:device{index}@example.net;extension={}>\r\n",
            "x".repeat(1400)
        );
        let output = registrar.request(peer, "address0", &header, None, index * 10)?;
        if response(&output)?.status_code.code() == 503 {
            assert!(output.events.is_empty());
            break;
        }
        accepted += 1;
        assert_eq!(contacts(&output)?.len(), accepted);
    }
    assert!((1..16).contains(&accepted));
    let query = registrar.request(peer, "address0", "", None, 200)?;
    assert_eq!(contacts(&query)?.len(), accepted);
    Ok(())
}
