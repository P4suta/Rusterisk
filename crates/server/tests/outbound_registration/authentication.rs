use super::{failure, finish, outcome, settings};
use rsipstack::sip::{HeadersExt, Method, Request, SipMessage};
use rusterisk_config::RegistrationAddress;
use rusterisk_server::{media::MediaHub, signaling::Signaling, ControlError, Controller};
use sha2::{Digest, Sha256};
use std::{net::SocketAddr, time::Duration};
use tokio::net::UdpSocket;

struct RegistrationPacket {
    request: Request,
    uri: String,
    peer: SocketAddr,
}

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

fn require(valid: bool, reason: &'static str) -> Result<(), ControlError> {
    if valid {
        Ok(())
    } else {
        Err(ControlError::Invalid(reason))
    }
}

async fn receive(peer: &UdpSocket) -> Result<RegistrationPacket, ControlError> {
    let mut bytes = [0; 16385];
    let (size, address) = tokio::time::timeout(Duration::from_secs(3), peer.recv_from(&mut bytes))
        .await
        .map_err(failure)?
        .map_err(failure)?;
    require(size <= 16384, "Oversized registration request")?;
    let source = std::str::from_utf8(&bytes[..size]).map_err(failure)?;
    let uri = source
        .split_once("\r\n")
        .and_then(|(line, _)| line.strip_prefix("REGISTER "))
        .and_then(|line| line.strip_suffix(" SIP/2.0"))
        .ok_or(ControlError::Invalid("Expected registration request line"))?
        .to_owned();
    let SipMessage::Request(request) = SipMessage::try_from(&bytes[..size]).map_err(failure)?
    else {
        return Err(ControlError::Invalid("Expected registration request"));
    };
    require(
        request.method == Method::Register,
        "Expected REGISTER method",
    )?;
    Ok(RegistrationPacket {
        request,
        uri,
        peer: address,
    })
}

async fn reply(
    peer: &UdpSocket,
    packet: &RegistrationPacket,
    status: u16,
    extra: &str,
) -> Result<(), ControlError> {
    let request = &packet.request;
    let response = format!(
        "SIP/2.0 {status} Response\r\nVia: {}\r\nFrom: {}\r\nTo: {};tag=registrar\r\nCall-ID: {}\r\nCSeq: {}\r\n{extra}Content-Length: 0\r\n\r\n",
        request.top_via_header().map_err(failure)?.value(),
        request.from_header().map_err(failure)?.value(),
        request.to_header().map_err(failure)?.value(),
        request.call_id_header().map_err(failure)?.value(),
        request.cseq_header().map_err(failure)?.value(),
    );
    let sent = peer
        .send_to(response.as_bytes(), packet.peer)
        .await
        .map_err(failure)?;
    require(sent == response.len(), "Incomplete registration response")
}

fn hash(source: &str) -> String {
    Sha256::digest(source.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn quoted<'a>(source: &'a str, name: &str) -> Result<&'a str, ControlError> {
    source
        .split_once(&format!("{name}=\""))
        .and_then(|(_, value)| value.split_once('"'))
        .map(|(value, _)| value)
        .ok_or(ControlError::Invalid("Missing authorization attribute"))
}

fn authenticate(
    packet: &RegistrationPacket,
    counter: u32,
    scope: Scope,
) -> Result<String, ControlError> {
    let value = packet
        .request
        .headers
        .iter()
        .find(|header| header.name() == scope.authorization())
        .ok_or(ControlError::Invalid("Missing registration authorization"))?
        .value();
    require(
        quoted(value, "username")? == "account",
        "Wrong authentication identity",
    )?;
    require(
        quoted(value, "realm")? == "provider.example",
        "Wrong authentication realm",
    )?;
    require(
        quoted(value, "nonce")? == "provider-nonce",
        "Wrong server nonce",
    )?;
    require(
        quoted(value, "opaque")? == "provider-opaque",
        "Missing opaque value",
    )?;
    require(
        quoted(value, "uri")? == packet.uri,
        "Digest changed wire Request-URI",
    )?;
    require(
        value.contains("algorithm=SHA-256, qop=auth-int"),
        "Wrong Digest profile",
    )?;
    require(
        value.contains(&format!("nc={counter:08x}")),
        "Wrong authentication nonce count",
    )?;
    require(
        packet.request.body.is_empty(),
        "REGISTER body was not empty",
    )?;
    let client = quoted(value, "cnonce")?;
    let a1 = hash("account:provider.example:sample");
    let a2 = hash(&format!("REGISTER:{}:{}", packet.uri, hash("")));
    let expected = hash(&format!(
        "{a1}:provider-nonce:{counter:08x}:{client}:auth-int:{a2}"
    ));
    require(
        quoted(value, "response")? == expected,
        "Registration entity digest mismatch",
    )?;
    Ok(client.to_owned())
}

#[tokio::test]
async fn configured_udp_registration_authenticates_and_refreshes_its_own_contact(
) -> Result<(), ControlError> {
    registration_flow(Scope::Origin).await
}

#[tokio::test]
async fn proxy_challenges_refresh_the_registered_contact_with_an_independent_scope(
) -> Result<(), ControlError> {
    registration_flow(Scope::Proxy).await
}

async fn registration_flow(scope: Scope) -> Result<(), ControlError> {
    let peer = UdpSocket::bind("127.0.0.1:0").await.map_err(failure)?;
    let remote = peer.local_addr().map_err(failure)?;
    let mut config = settings(remote)?;
    config
        .registrations
        .first_mut()
        .ok_or(ControlError::Capacity)?
        .client_uri = RegistrationAddress::parse("sip:line@provider.example").map_err(failure)?;
    let (controller, runner, _effects) = Controller::new(config.clone(), 16);
    let (media, _hub, notices) = MediaHub::new(16);
    let signaling = Signaling::bind(config, controller.clone(), runner, media, notices).await?;
    let stopped = controller.shutdown_signal();
    let signaling = tokio::spawn(signaling.run(stopped));
    let result: Result<(), ControlError> = async {
        let initial = receive(&peer).await?;
        require(initial.uri == format!("SIP:127.0.0.1:0{};transport=UDP;extension=%61b", remote.port()), "Initial wire Request-URI changed")?;
        require(initial.request.to_header().map_err(failure)?.value() == "<sip:line@provider.example>", "Address of record was replaced by authentication identity")?;
        reply(&peer, &initial, scope.status(), &format!("{}: Digest realm=\"provider.example\", nonce=\"provider-nonce\", algorithm=SHA-256, qop=\"auth,auth-int\", opaque=\"provider-opaque\"\r\n", scope.challenge())).await?;
        let authorized = receive(&peer).await?;
        let client_nonce = authenticate(&authorized, 1, scope)?;
        require(authorized.request.call_id_header().map_err(failure)?.value() == initial.request.call_id_header().map_err(failure)?.value(), "Authentication replaced boot Call-ID")?;
        let contact = authorized.request.contact_header().map_err(failure)?.value().to_owned();
        reply(&peer, &authorized, 200, &format!("Contact: {contact};expires=11\r\n")).await?;
        let refreshed = receive(&peer).await?;
        require(authenticate(&refreshed, 2, scope)? == client_nonce, "Refresh changed client nonce")?;
        require(refreshed.peer == initial.peer, "Refresh changed physical registrar route")?;
        require(refreshed.uri == initial.uri, "Refresh changed wire Request-URI")?;
        require(refreshed.request.contact_header().map_err(failure)?.value() == contact, "Refresh changed the owned Contact")?;
        require(refreshed.request.call_id_header().map_err(failure)?.value() == initial.request.call_id_header().map_err(failure)?.value(), "Refresh replaced boot Call-ID")?;
        require(refreshed.request.cseq_header().map_err(failure)?.value() != authorized.request.cseq_header().map_err(failure)?.value(), "Refresh reused request sequence")?;
        reply(&peer, &refreshed, 200, &format!("Contact: {contact};expires=11\r\n")).await?;
        Ok(())
    }.await;
    controller.shutdown();
    let signaling_result = finish(signaling).await;
    outcome(result, signaling_result)
}
