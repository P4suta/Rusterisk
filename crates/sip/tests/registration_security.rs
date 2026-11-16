use md5::Md5;
use rsipstack::sip::{headers::typed::WwwAuthenticate, HeadersExt, Response, SipMessage};
use rusterisk_config::{Config, Protocol};
use rusterisk_sip::{parse_registration_contacts, Engine, Output, RegistrationContacts, SipError};
use sha2::Digest;

struct Registrar {
    engine: Engine,
    token: &'static str,
    serial: u32,
}

impl Registrar {
    fn new(protocol: Protocol) -> Result<Self, SipError> {
        let (name, token, bind, files) = match protocol {
            Protocol::Tls => (
                "tls",
                "TLS",
                "127.0.0.1:5061",
                "cert_file=certificate.pem\npriv_key_file=private.pem\n",
            ),
            Protocol::Wss => ("wss", "WSS", "0.0.0.0", ""),
            Protocol::Udp | Protocol::Tcp | Protocol::Ws => return Err(SipError::Unsupported),
        };
        let http = if protocol == Protocol::Wss {
            "[general]\nbindaddr=127.0.0.1\ntlsenable=yes\ntlsbindaddr=127.0.0.1:8089\ntlscertfile=certificate.pem\ntlsprivatekey=private.pem\n"
        } else {
            "[general]\nbindaddr=127.0.0.1\n"
        };
        let config = Config::parse(
            http,
            "[app]\ntype=user\npassword=sample\n",
            &format!("[native]\ntype=transport\nprotocol={name}\nbind={bind}\n{files}[credential]\ntype=auth\nusername=account\npassword=sample\n[phone]\ntype=aor\nmax_contacts=4\nminimum_expiration=1\ndefault_expiration=120\n[phone-endpoint]\ntype=endpoint\ntransport=native\nauth=credential\naors=phone\n"),
            "",
            "",
        )?;
        Ok(Self {
            engine: Engine::new(config, [7; 32]).with_transport(protocol),
            token,
            serial: 0,
        })
    }

    fn request(&mut self, uri: &str, headers: &str, now: u64) -> Result<Output, SipError> {
        self.serial = self.serial.checked_add(1).ok_or(SipError::Capacity)?;
        let serial = self.serial;
        let peer = "127.0.0.2:5071".parse()?;
        let packet = |sequence: u32, authorization: &str| {
            format!("REGISTER {uri} SIP/2.0\r\nVia: SIP/2.0/{} {peer};branch=z9hG4bK-registration-security-{serial}-{sequence};rport\r\nFrom: <sip:displayed@provider.example>;tag=security-{serial}\r\nTo: <sip:phone@provider.example>\r\nCall-ID: registration-security-{serial}\r\nCSeq: {sequence} REGISTER\r\n{headers}Max-Forwards: 70\r\n{authorization}Content-Length: 0\r\n\r\n", self.token)
        };
        let first = self.engine.receive(packet(1, "").as_bytes(), peer, now)?;
        let challenged = response(&first)?;
        assert_eq!(challenged.status_code.code(), 401);
        let challenge = WwwAuthenticate::parse(
            challenged
                .www_authenticate_header()
                .ok_or(SipError::InvalidPacket)?
                .value(),
        )?;
        let cnonce = format!("security-client-{serial}");
        let first = hash(&format!("account:{}:sample", challenge.realm));
        let second = hash(&format!("REGISTER:{uri}"));
        let digest = hash(&format!(
            "{first}:{}:00000001:{cnonce}:auth:{second}",
            challenge.nonce
        ));
        let authorization = format!("Authorization: Digest username=\"account\", realm=\"{}\", nonce=\"{}\", uri=\"{uri}\", response=\"{digest}\", algorithm=MD5, qop=auth, nc=00000001, cnonce=\"{cnonce}\"\r\n", challenge.realm, challenge.nonce);
        self.engine
            .receive(packet(2, &authorization).as_bytes(), peer, now + 1)
    }
}

fn hash(source: &str) -> String {
    Md5::digest(source.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn response(output: &Output) -> Result<Response, SipError> {
    let packet = output.datagrams.first().ok_or(SipError::InvalidPacket)?;
    match SipMessage::try_from(packet.bytes.as_slice())? {
        SipMessage::Response(response) => Ok(response),
        SipMessage::Request(_) => Err(SipError::InvalidPacket),
    }
}

fn contacts(output: &Output) -> Result<Vec<String>, SipError> {
    let response = response(output)?;
    assert_eq!(response.status_code.code(), 200);
    match parse_registration_contacts(
        response
            .headers
            .iter()
            .filter(|header| header.name().eq_ignore_ascii_case("Contact"))
            .map(|header| header.value()),
        None,
    )
    .map_err(|_| SipError::InvalidPacket)?
    {
        RegistrationContacts::Query => Ok(Vec::new()),
        RegistrationContacts::Bindings(contacts) => Ok(contacts
            .as_slice()
            .iter()
            .map(|contact| contact.source_uri().into())
            .collect()),
        RegistrationContacts::RemoveAll => Err(SipError::InvalidPacket),
    }
}

#[test]
fn secure_bindings_accept_secure_registrar_contacts_and_paths_without_testing_aor_schemes(
) -> Result<(), SipError> {
    for protocol in [Protocol::Tls, Protocol::Wss] {
        for path in [
            "",
            "Path: <sips:first@one.invalid;lr>, <sips:second@two.invalid;lr>\r\n",
            "Path: <sips:first@one.invalid;lr>\r\nPath: <sips:second@two.invalid;lr>\r\n",
        ] {
            let mut registrar = Registrar::new(protocol)?;
            let result = registrar.request(
                "sips:provider.example",
                &format!("Contact: <sips:device@peer.invalid>\r\n{path}"),
                0,
            )?;
            assert_eq!(contacts(&result)?, ["sips:device@peer.invalid"]);
        }
    }
    Ok(())
}

#[test]
fn sips_bindings_require_secure_registrar_and_every_path_uri() -> Result<(), SipError> {
    for (uri, path) in [
        ("sip:provider.example", ""),
        (
            "sips:provider.example",
            "Path: <sip:proxy@one.invalid;lr>\r\n",
        ),
        (
            "sips:provider.example",
            "Path: <sips:first@one.invalid;lr>, <sip:second@two.invalid;lr>\r\n",
        ),
        (
            "sips:provider.example",
            "Path: <sips:first@one.invalid;lr>\r\nPath: <sip:second@two.invalid;lr>\r\n",
        ),
    ] {
        let mut registrar = Registrar::new(Protocol::Tls)?;
        let result = registrar.request(
            uri,
            &format!("Contact: <sips:device@peer.invalid>\r\n{path}"),
            0,
        )?;
        assert_eq!(response(&result)?.status_code.code(), 400, "{uri}: {path}");
        assert!(result.events.is_empty());
        assert!(contacts(&registrar.request("sip:provider.example", "", 10)?)?.is_empty());
    }
    Ok(())
}

#[test]
fn secure_request_targets_cannot_bind_insecure_contacts() -> Result<(), SipError> {
    let mut registrar = Registrar::new(Protocol::Tls)?;
    let result = registrar.request(
        "sips:provider.example",
        "Contact: <sip:device@peer.invalid>\r\n",
        0,
    )?;
    assert_eq!(response(&result)?.status_code.code(), 400);
    assert!(result.events.is_empty());
    Ok(())
}

#[test]
fn inconsistent_contact_sets_leave_prior_bindings_and_presence_unchanged() -> Result<(), SipError> {
    let mut registrar = Registrar::new(Protocol::Tls)?;
    let seeded = registrar.request(
        "sip:provider.example",
        "Contact: <sip:original@peer.invalid>\r\n",
        0,
    )?;
    assert_eq!(contacts(&seeded)?, ["sip:original@peer.invalid"]);
    let rejected = registrar.request(
        "sips:provider.example",
        "Contact: <sip:original@peer.invalid>;expires=0, <sips:replacement@peer.invalid>\r\n",
        10,
    )?;
    assert_eq!(response(&rejected)?.status_code.code(), 400);
    assert!(rejected.events.is_empty());
    let queried = registrar.request("sip:provider.example", "", 20)?;
    assert_eq!(contacts(&queried)?, ["sip:original@peer.invalid"]);
    assert!(queried.events.is_empty());
    assert_eq!(
        registrar.engine.registered_peer("phone-endpoint", 22),
        Some("127.0.0.2:5071".parse()?)
    );
    Ok(())
}

#[test]
fn path_validation_rejects_invalid_routes_without_publishing_a_binding() -> Result<(), SipError> {
    for path in [
        "Path: *\r\n",
        "Path: <sips:proxy@one.invalid\r\n",
        "Path: <https:proxy@one.invalid>\r\n",
    ] {
        let mut registrar = Registrar::new(Protocol::Tls)?;
        let result = registrar.request(
            "sips:provider.example",
            &format!("Contact: <sips:device@peer.invalid>\r\n{path}"),
            0,
        )?;
        assert_eq!(response(&result)?.status_code.code(), 400);
        assert!(result.events.is_empty());
        assert!(contacts(&registrar.request("sip:provider.example", "", 10)?)?.is_empty());
    }
    Ok(())
}

#[test]
fn path_capacity_is_checked_before_replacing_a_secure_binding() -> Result<(), SipError> {
    let mut registrar = Registrar::new(Protocol::Tls)?;
    for (count, expected) in [(32, 200), (33, 400)] {
        let paths = (0..count)
            .map(|index| format!("<sips:proxy{index}@route.invalid;lr>"))
            .collect::<Vec<_>>()
            .join(", ");
        let result = registrar.request(
            "sips:provider.example",
            &format!("Contact: <sips:device@peer.invalid>\r\nPath: {paths}\r\n"),
            count,
        )?;
        assert_eq!(response(&result)?.status_code.code(), expected);
        if expected == 400 {
            assert!(result.events.is_empty());
        }
    }
    let queried = registrar.request("sip:provider.example", "", 100)?;
    assert_eq!(contacts(&queried)?, ["sips:device@peer.invalid"]);
    assert!(queried.events.is_empty());
    Ok(())
}

#[test]
fn path_entries_require_name_address_syntax() -> Result<(), SipError> {
    let mut registrar = Registrar::new(Protocol::Tls)?;
    let result = registrar.request(
        "sips:provider.example",
        "Contact: <sips:device@peer.invalid>\r\nPath: sips:proxy@route.invalid;lr\r\n",
        0,
    )?;
    assert_eq!(response(&result)?.status_code.code(), 400);
    assert!(result.events.is_empty());
    Ok(())
}

#[test]
fn path_entries_require_a_loose_routing_indicator() -> Result<(), SipError> {
    let mut registrar = Registrar::new(Protocol::Tls)?;
    let result = registrar.request(
        "sips:provider.example",
        "Contact: <sips:device@peer.invalid>\r\nPath: <sips:proxy@route.invalid>\r\n",
        0,
    )?;
    assert_eq!(response(&result)?.status_code.code(), 400);
    assert!(result.events.is_empty());
    Ok(())
}
