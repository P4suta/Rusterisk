use super::{config, dns, stream::reply};
use crate::{
    media::MediaHub,
    signaling::{cleanup, resolver::DestinationResolver, transport::Input, Signaling},
    ControlError, Controller,
};
use hickory_resolver::proto::{
    op::{Message, OpCode},
    rr::{
        rdata::{A, SRV},
        Name, RData, Record, RecordType,
    },
};
use rsipstack::sip::{headers::untyped::ToTypedHeader, HeadersExt, Method, Request, SipMessage};
use rusterisk_config::{Protocol, RegistrarUri};
use rusterisk_sip::StaticTarget;
use sha2::{Digest, Sha256};
use std::{
    net::{Ipv4Addr, SocketAddr},
    time::Duration,
};
use tokio::net::UdpSocket;
use tokio::time::Instant;
#[path = "service_unavailable/exhaustion.rs"]
mod exhaustion;
#[path = "service_unavailable/persistent_wait.rs"]
mod persistent_wait;

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}

fn require(valid: bool, reason: &'static str) -> Result<(), ControlError> {
    if valid {
        Ok(())
    } else {
        Err(ControlError::Invalid(reason))
    }
}

async fn answer(
    socket: &UdpSocket,
    name: &Name,
    kind: RecordType,
    records: Vec<Record>,
) -> Result<(), ControlError> {
    let mut bytes = [0; 1233];
    let (size, peer) = tokio::time::timeout(Duration::from_secs(1), socket.recv_from(&mut bytes))
        .await
        .map_err(failure)?
        .map_err(failure)?;
    require(size <= 1232, "Oversized registration candidate DNS request")?;
    let request = Message::from_vec(&bytes[..size]).map_err(failure)?;
    require(
        request.queries.len() == 1,
        "Expected one candidate DNS question",
    )?;
    let query = request.queries.first().ok_or(ControlError::Capacity)?;
    require(
        query.name() == name && query.query_type() == kind,
        "Registration candidate DNS order changed",
    )?;
    let mut response = Message::response(request.metadata.id, OpCode::Query);
    response.metadata.authoritative = true;
    response.metadata.recursion_available = true;
    response.metadata.recursion_desired = request.metadata.recursion_desired;
    response.queries.push(query.clone());
    response.answers = records;
    response.edns = request.edns;
    let bytes = response.to_vec().map_err(failure)?;
    require(bytes.len() <= 1232, "Oversized candidate DNS response")?;
    dns::send(socket, &bytes, peer).await
}

async fn candidates(
    socket: &UdpSocket,
    primary: SocketAddr,
    backup: SocketAddr,
) -> Result<(), ControlError> {
    let service = Name::from_ascii("_sip._udp.provider.test.").map_err(failure)?;
    let records = [
        (0, primary.port(), "primary.provider.test."),
        (1, backup.port(), "backup.provider.test."),
    ]
    .into_iter()
    .map(|(priority, port, target)| {
        Ok(Record::from_rdata(
            service.clone(),
            60,
            RData::SRV(SRV::new(
                priority,
                0,
                port,
                Name::from_ascii(target).map_err(failure)?,
            )),
        ))
    })
    .collect::<Result<Vec<_>, ControlError>>()?;
    answer(socket, &service, RecordType::SRV, records).await?;
    for host in ["primary.provider.test.", "backup.provider.test."] {
        let name = Name::from_ascii(host).map_err(failure)?;
        let records = std::iter::once(Record::from_rdata(
            name.clone(),
            60,
            RData::A(A(Ipv4Addr::LOCALHOST)),
        ))
        .collect();
        answer(socket, &name, RecordType::A, records).await?;
    }
    Ok(())
}

struct Packet {
    request: Request,
    uri: String,
    peer: SocketAddr,
}

fn header<'a>(packet: &'a Packet, name: &str) -> Result<&'a str, ControlError> {
    packet
        .request
        .headers
        .iter()
        .find(|header| header.name().eq_ignore_ascii_case(name))
        .map(|header| header.value())
        .ok_or(ControlError::Invalid("Missing candidate identity header"))
}

async fn receive(socket: &UdpSocket, phase: &'static str) -> Result<Packet, ControlError> {
    let mut bytes = [0; 16_385];
    let (size, peer) =
        tokio::time::timeout(Duration::from_millis(250), socket.recv_from(&mut bytes))
            .await
            .map_err(|error| failure(format!("{phase}: {error}")))?
            .map_err(failure)?;
    require(size <= 16_384, "Oversized candidate REGISTER request")?;
    let source = std::str::from_utf8(&bytes[..size]).map_err(failure)?;
    let uri = source
        .split_once("\r\n")
        .and_then(|(line, _)| line.strip_prefix("REGISTER "))
        .and_then(|line| line.strip_suffix(" SIP/2.0"))
        .ok_or(ControlError::Invalid(
            "Expected candidate REGISTER request line",
        ))?
        .to_owned();
    let SipMessage::Request(request) = SipMessage::try_from(&bytes[..size]).map_err(failure)?
    else {
        return Err(ControlError::Invalid("Expected candidate REGISTER request"));
    };
    require(
        request.method == Method::Register,
        "Expected REGISTER method",
    )?;
    Ok(Packet { request, uri, peer })
}

fn successor(
    original: &Packet,
    previous: &[&Packet],
    next: &Packet,
    sequence: u32,
) -> Result<(), ControlError> {
    require(
        next.peer == original.peer && next.uri == original.uri,
        "Candidate selection changed the local route or wire URI",
    )?;
    for name in ["Call-ID", "From", "To", "Contact"] {
        require(
            header(next, name)? == header(original, name)?,
            "Candidate selection changed the registration identity",
        )?;
    }
    require(
        next.request.cseq_header().map_err(failure)?.value() == format!("{sequence} REGISTER"),
        "Candidate REGISTER did not increment CSeq exactly once",
    )?;
    let via = next
        .request
        .top_via_header()
        .map_err(failure)?
        .typed()
        .map_err(failure)?;
    require(
        via.branch().is_some(),
        "Candidate REGISTER omitted its branch",
    )?;
    for packet in previous {
        let previous = packet
            .request
            .top_via_header()
            .map_err(failure)?
            .typed()
            .map_err(failure)?;
        require(
            via.branch() != previous.branch(),
            "Candidate REGISTER reused a branch",
        )?;
    }
    Ok(())
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
        .ok_or(ControlError::Invalid(
            "Missing backup authorization attribute",
        ))
}

fn authorized(packet: &Packet, count: u32) -> Result<String, ControlError> {
    let value = packet
        .request
        .authorization_header()
        .ok_or(ControlError::Invalid("Missing backup authorization"))?
        .value();
    let cnonce = quoted(value, "cnonce")?;
    let expected = hash(&format!(
        "{}:backup-nonce:{count:08x}:{cnonce}:auth-int:{}",
        hash("account:backup.provider.test:sample"),
        hash(&format!("REGISTER:{}:{}", packet.uri, hash("")))
    ));
    require(
        quoted(value, "username")? == "account"
            && quoted(value, "realm")? == "backup.provider.test"
            && quoted(value, "nonce")? == "backup-nonce"
            && quoted(value, "opaque")? == "backup-opaque"
            && quoted(value, "uri")? == packet.uri
            && quoted(value, "response")? == expected
            && value.contains("algorithm=SHA-256")
            && value.contains("qop=auth-int")
            && value.contains(&format!("nc={count:08x}"))
            && packet.request.body.is_empty(),
        "Backup authentication changed its identity, nonce, or wire digest",
    )?;
    Ok(cnonce.to_owned())
}

async fn respond(
    signaling: &mut Signaling,
    socket: &UdpSocket,
    packet: &Packet,
    status: u16,
    extra: &str,
    now: u64,
) -> Result<(), ControlError> {
    dns::send(
        socket,
        reply(&packet.request, status, extra)?.as_bytes(),
        packet.peer,
    )
    .await?;
    let Input::Packet(received) =
        tokio::time::timeout(Duration::from_millis(250), signaling.network.next())
            .await
            .map_err(failure)??
    else {
        return Err(ControlError::Invalid(
            "Expected candidate registration response",
        ));
    };
    require(
        received.peer == socket.local_addr().map_err(failure)?
            && signaling.registration_received(&received, now),
        "Candidate registrar response was not consumed",
    )
}

async fn silent(socket: &UdpSocket) -> Result<(), ControlError> {
    match tokio::time::timeout(
        Duration::from_millis(30),
        socket.recv_from(&mut [0; 16_385]),
    )
    .await
    {
        Err(_) => Ok(()),
        Ok(Err(error)) => Err(failure(error)),
        Ok(Ok(_)) => Err(ControlError::Invalid(
            "Failed primary received another REGISTER before Retry-After",
        )),
    }
}

async fn finish(mut signaling: Signaling, local: SocketAddr) -> Result<(), ControlError> {
    let mut result = signaling.finish_registrations().await;
    result = cleanup::combine(result, signaling.finish_native().await);
    result = cleanup::combine(result, signaling.network.close().await);
    drop(signaling);
    let rebound = UdpSocket::bind(local)
        .await
        .map(|_socket| ())
        .map_err(failure);
    cleanup::combine(result, rebound)
}

#[tokio::test]
async fn a_service_unavailable_response_advances_to_the_next_resolved_registrar(
) -> Result<(), ControlError> {
    let primary = UdpSocket::bind("127.0.0.1:0").await.map_err(failure)?;
    let backup = UdpSocket::bind("127.0.0.1:0").await.map_err(failure)?;
    let dns_socket = UdpSocket::bind("127.0.0.1:0").await.map_err(failure)?;
    let resolver: DestinationResolver = dns::resolver(&dns_socket)?;
    let primary_address = primary.local_addr().map_err(failure)?;
    let backup_address = backup.local_addr().map_err(failure)?;
    let uri = "SIP:provider.test;transport=UDP;extension=%61b";
    let target = StaticTarget::from_uri(Protocol::Udp, uri).map_err(failure)?;
    let (resolution, answered) = tokio::join!(
        resolver.resolve(&target),
        candidates(&dns_socket, primary_address, backup_address),
    );
    let resolved = match (resolution, answered) {
        (Ok(resolved), Ok(())) => resolved,
        (Err(error), Ok(())) | (Ok(_), Err(error)) => return Err(error),
        (Err(primary), Err(cleanup)) => {
            return Err(failure(format!(
                "{primary}; candidate DNS failed: {cleanup}"
            )))
        }
    };
    require(
        resolved.peers().eq([primary_address, backup_address])
            && resolved.identity() == "provider.test"
            && resolved.cache_valid(Instant::now()),
        "Candidate DNS precondition failed",
    )?;
    drop(dns_socket);
    let mut settings = config(&["provider.test"], 0)?;
    settings
        .auth
        .first_mut()
        .ok_or(ControlError::Capacity)?
        .realm = "backup.provider.test".into();
    let registration = settings
        .registrations
        .first_mut()
        .ok_or(ControlError::Capacity)?;
    registration.server_uri = RegistrarUri::parse(Protocol::Udp, uri).map_err(failure)?;
    registration.expiration = 11;
    registration.retry_interval = 60;
    registration.max_retries = 0;
    let (controller, _runner, effects) = Controller::new(settings.clone(), 16);
    let (media, _hub, notices) = MediaHub::new(16);
    let mut signaling = Signaling::bind(settings, controller, effects, media, notices).await?;
    signaling.resolver = Ok(resolver);
    signaling
        .resolution_cache
        .insert(resolved, Instant::now())?;
    let local = signaling
        .listeners()?
        .first()
        .ok_or(ControlError::Capacity)?
        .1;
    let result = async {
        signaling.registration_tick(1000).await?;
        let completion = tokio::time::timeout(Duration::from_secs(1), signaling.registrations.completed())
            .await.map_err(failure)??.ok_or(ControlError::Invalid("Candidate resolution job disappeared"))?;
        signaling.registration_completed(completion, 1000)?;
        signaling.registration_tick(1000).await?;
        let initial = receive(&primary, "Initial primary REGISTER").await?;
        require(initial.peer == local && initial.uri == uri, "Initial candidate route or wire URI changed")?;
        require(initial.request.cseq_header().map_err(failure)?.value() == "1 REGISTER", "Initial candidate CSeq changed")?;
        respond(&mut signaling, &primary, &initial, 503, "Retry-After: 10\r\n", 1100).await?;
        signaling.registration_tick(1200).await?;
        let (alternate, quiet) = tokio::join!(
            receive(&backup, "Backup REGISTER after checked primary 503"),
            silent(&primary),
        );
        let alternate = match (alternate, quiet) {
            (Ok(alternate), Ok(())) => alternate,
            (Err(error), Ok(())) | (Ok(_), Err(error)) => return Err(error),
            (Err(primary), Err(quiet)) => return Err(failure(format!("{primary}; primary silence failed: {quiet}"))),
        };
        successor(&initial, &[&initial], &alternate, 2)?;
        respond(&mut signaling, &backup, &alternate, 401, "WWW-Authenticate: Digest realm=\"backup.provider.test\", nonce=\"backup-nonce\", algorithm=SHA-256, qop=\"auth-int\", opaque=\"backup-opaque\"\r\n", 1300).await?;
        signaling.registration_tick(1400).await?;
        let authorized_packet = receive(&backup, "Authenticated backup REGISTER").await?;
        successor(&initial, &[&initial, &alternate], &authorized_packet, 3)?;
        let cnonce = authorized(&authorized_packet, 1)?;
        let contact = authorized_packet.request.contact_header().map_err(failure)?.value();
        respond(&mut signaling, &backup, &authorized_packet, 200, &format!("Contact: {contact};expires=11\r\n"), 1500).await?;
        signaling.registration_tick(2500).await?;
        let refresh = receive(&backup, "Successful backup registration refresh").await?;
        successor(&initial, &[&initial, &alternate, &authorized_packet], &refresh, 4)?;
        require(authorized(&refresh, 2)? == cnonce, "Backup refresh changed client nonce")?;
        respond(&mut signaling, &backup, &refresh, 200, &format!("Contact: {contact};expires=11\r\n"), 2600).await?;
        silent(&primary).await
    }.await;
    cleanup::combine(result, finish(signaling, local).await)
}
