use super::{config, ControlError};
use crate::{
    media::MediaHub,
    signaling::{resolver::DestinationResolver, Signaling},
    Controller,
};
use hickory_resolver::{
    config::{LookupIpStrategy, NameServerConfig, ResolveHosts, ResolverConfig, ResolverOpts},
    proto::{
        op::{Message, OpCode},
        rr::{rdata::A, RData, Record},
    },
};
use rsipstack::sip::{HeadersExt, Method, SipMessage};
use rusterisk_config::{Protocol, RegistrarUri};
use std::{net::Ipv4Addr, time::Duration};
use tokio::{net::UdpSocket, task::JoinHandle};

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}

async fn finish(mut task: JoinHandle<Result<(), ControlError>>) -> Result<(), ControlError> {
    match tokio::time::timeout(Duration::from_secs(2), &mut task).await {
        Ok(result) => result.map_err(failure)?,
        Err(timeout) => {
            task.abort();
            match task.await {
                Err(error) if error.is_cancelled() => Err(failure(timeout)),
                Err(error) => Err(failure(error)),
                Ok(Err(error)) => Err(error),
                Ok(Ok(())) => Err(failure(timeout)),
            }
        }
    }
}

pub(super) fn resolver(dns: &UdpSocket) -> Result<DestinationResolver, ControlError> {
    let mut server = NameServerConfig::udp(Ipv4Addr::LOCALHOST.into());
    server
        .connections
        .first_mut()
        .ok_or(ControlError::Capacity)?
        .port = dns.local_addr().map_err(failure)?.port();
    let mut options = ResolverOpts::default();
    options.ip_strategy = LookupIpStrategy::Ipv4Only;
    options.use_hosts_file = ResolveHosts::Never;
    DestinationResolver::with_config(
        ResolverConfig::from_name_servers(std::iter::once(server).collect()),
        options,
    )
}

pub(super) async fn receive(
    socket: &UdpSocket,
) -> Result<(Vec<u8>, std::net::SocketAddr), ControlError> {
    let mut bytes = [0; 16_385];
    let (size, peer) = tokio::time::timeout(Duration::from_secs(3), socket.recv_from(&mut bytes))
        .await
        .map_err(failure)?
        .map_err(failure)?;
    Ok((bytes[..size].to_vec(), peer))
}

pub(super) async fn send(
    socket: &UdpSocket,
    bytes: &[u8],
    peer: std::net::SocketAddr,
) -> Result<(), ControlError> {
    if socket.send_to(bytes, peer).await.map_err(failure)? != bytes.len() {
        return Err(ControlError::Invalid("Incomplete test datagram"));
    }
    Ok(())
}

#[tokio::test]
async fn registration_resolution_preserves_signaling_progress_and_request_uri_octets(
) -> Result<(), ControlError> {
    let dns = UdpSocket::bind("127.0.0.1:0").await.map_err(failure)?;
    let registrar = UdpSocket::bind("127.0.0.1:0").await.map_err(failure)?;
    let remote = registrar.local_addr().map_err(failure)?;
    let mut config = config(&[&format!("provider.test:{}", remote.port())], 0)?;
    let uri = format!(
        "SIP:provider.test:0{};transport=UDP;extension=%61b",
        remote.port()
    );
    config
        .registrations
        .first_mut()
        .ok_or(ControlError::Capacity)?
        .server_uri = RegistrarUri::parse(Protocol::Udp, &uri).map_err(failure)?;
    let (controller, runner, _effects) = Controller::new(config.clone(), 16);
    let (media, _hub, notices) = MediaHub::new(16);
    let mut signaling = Signaling::bind(config, controller.clone(), runner, media, notices).await?;
    signaling.resolver = Ok(resolver(&dns)?);
    let local = signaling
        .listeners()?
        .first()
        .ok_or(ControlError::Capacity)?
        .1;
    let stopped = controller.shutdown_signal();
    let task = tokio::spawn(signaling.run(stopped));
    let primary: Result<(), ControlError> = async {
        let (query, dns_peer) = receive(&dns).await?;
        let query = Message::from_vec(&query).map_err(failure)?;
        let question = query.queries.first().ok_or(ControlError::Capacity)?.clone();
        if question.name().to_ascii() != "provider.test." {
            return Err(ControlError::Invalid("Registration queried another name"));
        }
        let options = format!("OPTIONS sip:rusterisk@{local} SIP/2.0\r\nVia: SIP/2.0/UDP {remote};branch=z9hG4bK-dns-progress;rport\r\nFrom: <sip:peer@provider.test>;tag=peer\r\nTo: <sip:rusterisk@{local}>\r\nCall-ID: registration-resolution-progress\r\nCSeq: 1 OPTIONS\r\nMax-Forwards: 70\r\nContent-Length: 0\r\n\r\n");
        send(&registrar, options.as_bytes(), local).await?;
        let (bytes, address) = receive(&registrar).await?;
        let SipMessage::Response(response) =
            SipMessage::try_from(bytes.as_slice()).map_err(failure)?
        else {
            return Err(ControlError::Invalid("Expected OPTIONS response"));
        };
        if address != local || response.status_code.code() != 200 {
            return Err(ControlError::Invalid("Signaling stopped during resolution"));
        }
        let mut response = Message::response(query.metadata.id, OpCode::Query);
        response.metadata.authoritative = true;
        response.metadata.recursion_available = true;
        response.metadata.recursion_desired = query.metadata.recursion_desired;
        response.queries.push(question.clone());
        response.answers.push(Record::from_rdata(
            question.name().clone(),
            60,
            RData::A(A(Ipv4Addr::LOCALHOST)),
        ));
        response.edns = query.edns;
        send(&dns, &response.to_vec().map_err(failure)?, dns_peer).await?;
        let (bytes, address) = receive(&registrar).await?;
        let source = std::str::from_utf8(&bytes).map_err(failure)?;
        if !source.starts_with(&format!("REGISTER {uri} SIP/2.0\r\n")) {
            return Err(ControlError::Invalid("Resolution changed Request-URI octets"));
        }
        let SipMessage::Request(request) =
            SipMessage::try_from(bytes.as_slice()).map_err(failure)?
        else {
            return Err(ControlError::Invalid("Expected resolved REGISTER request"));
        };
        if address != local || request.method != Method::Register {
            return Err(ControlError::Invalid("Registration used another signaling route"));
        }
        let contact = request.contact_header().map_err(failure)?.value();
        if contact != format!("<sip:s@{local};transport=udp>") {
            return Err(ControlError::Invalid("Registration advertised another Contact"));
        }
        let granted = format!("SIP/2.0 200 OK\r\nVia: {}\r\nFrom: {}\r\nTo: {};tag=registrar\r\nCall-ID: {}\r\nCSeq: {}\r\nContact: {contact};expires=11\r\nExpires: 11\r\nContent-Length: 0\r\n\r\n", request.top_via_header().map_err(failure)?.value(), request.from_header().map_err(failure)?.value(), request.to_header().map_err(failure)?.value(), request.call_id_header().map_err(failure)?.value(), request.cseq_header().map_err(failure)?.value());
        send(&registrar, granted.as_bytes(), address).await?;
        let (bytes, address) = receive(&registrar).await?;
        let source = std::str::from_utf8(&bytes).map_err(failure)?;
        let SipMessage::Request(refresh) =
            SipMessage::try_from(bytes.as_slice()).map_err(failure)?
        else {
            return Err(ControlError::Invalid("Expected registration refresh"));
        };
        if !source.starts_with(&format!("REGISTER {uri} SIP/2.0\r\n"))
            || address != local
            || refresh.cseq_header().map_err(failure)?.value() != "2 REGISTER"
            || refresh.call_id_header().map_err(failure)?.value()
                != request.call_id_header().map_err(failure)?.value()
            || refresh.contact_header().map_err(failure)?.value() != contact
        {
            return Err(ControlError::Invalid("Refresh changed the resolved registration"));
        }
        Ok(())
    }
    .await;
    controller.shutdown();
    let cleanup = finish(task).await;
    super::super::super::cleanup::combine(primary, cleanup)?;
    let _rebound = UdpSocket::bind(local).await.map_err(failure)?;
    Ok(())
}
