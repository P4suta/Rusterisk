use super::{add_registration, finish, requests};
use crate::{
    encryption::TlsContext,
    media::MediaHub,
    signaling::{
        cleanup,
        registrations::tests::{dns, stream, tls},
        Signaling,
    },
    ControlError, Controller,
};
use hickory_resolver::proto::{
    op::{Message, OpCode},
    rr::{
        rdata::{A, SRV},
        Name, RData, Record, RecordType,
    },
};
use rusterisk_config::{Protocol, RegistrarUri};
use rusterisk_sip::StaticTarget;
use std::{net::Ipv4Addr, time::Duration};
use tokio::time::Instant;
use tokio::{
    net::{TcpListener, TcpSocket, UdpSocket},
    sync::watch,
};

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}

async fn answer(
    socket: UdpSocket,
    preferred: u16,
    backup: u16,
    mut stopped: watch::Receiver<bool>,
) -> Result<(), ControlError> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(12);
    let mut bytes = [0; 1233];
    for _ in 0..16 {
        let (size, peer) = tokio::select! {
            biased;
            _changed = stopped.changed() => return Ok(()),
            packet = socket.recv_from(&mut bytes) => packet.map_err(failure)?,
            () = tokio::time::sleep_until(deadline) => return Err(ControlError::Invalid("Fallback DNS fixture exceeded its lifetime")),
        };
        if size > 1232 {
            return Err(ControlError::Capacity);
        }
        let request = Message::from_vec(&bytes[..size]).map_err(failure)?;
        let query = request.queries.first().ok_or(ControlError::Capacity)?;
        let mut response = Message::response(request.metadata.id, OpCode::Query);
        response.metadata.authoritative = true;
        response.metadata.recursion_available = true;
        response.metadata.recursion_desired = request.metadata.recursion_desired;
        response.queries.push(query.clone());
        response.edns = request.edns;
        response.answers = match query.query_type() {
            RecordType::SRV => [
                (0, preferred, "primary.provider.test."),
                (1, backup, "backup.provider.test."),
            ]
            .into_iter()
            .map(|(priority, port, target)| {
                Ok(Record::from_rdata(
                    query.name().clone(),
                    60,
                    RData::SRV(SRV::new(
                        priority,
                        0,
                        port,
                        Name::from_ascii(target).map_err(failure)?,
                    )),
                ))
            })
            .collect::<Result<Vec<_>, ControlError>>()?,
            RecordType::A => std::iter::once(Record::from_rdata(
                query.name().clone(),
                60,
                RData::A(A(Ipv4Addr::LOCALHOST)),
            ))
            .collect(),
            RecordType::AAAA => Vec::new(),
            RecordType::ANAME
            | RecordType::ANY
            | RecordType::AXFR
            | RecordType::CAA
            | RecordType::CDNSKEY
            | RecordType::CDS
            | RecordType::CERT
            | RecordType::CNAME
            | RecordType::CSYNC
            | RecordType::DNAME
            | RecordType::DNSKEY
            | RecordType::DS
            | RecordType::HINFO
            | RecordType::HTTPS
            | RecordType::IXFR
            | RecordType::KEY
            | RecordType::MX
            | RecordType::NAPTR
            | RecordType::NS
            | RecordType::NSEC
            | RecordType::NSEC3
            | RecordType::NSEC3PARAM
            | RecordType::NULL
            | RecordType::OPENPGPKEY
            | RecordType::OPT
            | RecordType::PTR
            | RecordType::RRSIG
            | RecordType::SIG
            | RecordType::SMIMEA
            | RecordType::SOA
            | RecordType::SSHFP
            | RecordType::SVCB
            | RecordType::TLSA
            | RecordType::TSIG
            | RecordType::TXT
            | RecordType::Unknown(_)
            | RecordType::ZERO
            | _ => {
                return Err(ControlError::Invalid(
                    "Unexpected fallback fixture DNS query",
                ))
            }
        };
        dns::send(&socket, &response.to_vec().map_err(failure)?, peer).await?;
    }
    Err(ControlError::Capacity)
}

async fn registration(protocol: Protocol) -> Result<(), ControlError> {
    let preferred = TcpSocket::new_v4().map_err(failure)?;
    preferred
        .bind("127.0.0.1:0".parse().map_err(failure)?)
        .map_err(failure)?;
    let preferred_address = preferred.local_addr().map_err(failure)?;
    let preferred_port = preferred_address.port();
    let registrar = TcpListener::bind("127.0.0.1:0").await.map_err(failure)?;
    let backup_address = registrar.local_addr().map_err(failure)?;
    let backup_port = backup_address.port();
    let dns = UdpSocket::bind("127.0.0.1:0").await.map_err(failure)?;
    let resolver = dns::resolver(&dns)?;
    let identity = (protocol == Protocol::Tls)
        .then(|| tls::Identity::new("provider.test"))
        .transpose()?;
    let (mut config, _numeric) = match &identity {
        Some(identity) => {
            tls::configuration(registrar.local_addr().map_err(failure)?, identity, identity)?
        }
        None => (
            stream::configuration("sip:provider.test;transport=tcp")?,
            String::new(),
        ),
    };
    let uri = format!(
        "{}:provider.test;transport=TCP;extension=%61b",
        if protocol == Protocol::Tls {
            "SIPS"
        } else {
            "SIP"
        }
    );
    let target = StaticTarget::from_uri(protocol, &uri).map_err(failure)?;
    config
        .registrations
        .first_mut()
        .ok_or(ControlError::Capacity)?
        .server_uri = RegistrarUri::parse(protocol, &uri).map_err(failure)?;
    add_registration(&mut config)?;
    let context = match &identity {
        Some(identity) => Some(TlsContext::load(&identity.files).await?),
        None => None,
    };
    let (controller, runner, _effects) = Controller::new(config.clone(), 16);
    let (media, _hub, notices) = MediaHub::new(16);
    let mut signaling = Signaling::bind(config, controller.clone(), runner, media, notices).await?;
    signaling.resolver = Ok(resolver.clone());
    let local = signaling
        .listeners()?
        .first()
        .ok_or(ControlError::Capacity)?
        .1;
    let (stop, stopped) = watch::channel(false);
    let dns_task = tokio::spawn(answer(dns, preferred_port, backup_port, stopped));
    let resolution = resolver
        .resolve(&target)
        .await
        .map_err(|error| ControlError::Adapter(format!("Fallback DNS lookup: {error}")));
    stop.send_replace(true);
    let closed = finish(dns_task)
        .await
        .map_err(|error| ControlError::Adapter(format!("Fallback DNS cleanup: {error}")));
    let resolved = match (resolution, closed) {
        (Ok(resolved), Ok(())) => resolved,
        (Err(error), Ok(())) | (Ok(_), Err(error)) => return Err(error),
        (Err(primary), Err(cleanup)) => {
            return Err(ControlError::Adapter(format!("{primary}; {cleanup}")))
        }
    };
    if !resolved.peers().eq([preferred_address, backup_address])
        || resolved.identity() != "provider.test"
        || !resolved.cache_valid(Instant::now())
    {
        return Err(ControlError::Invalid(
            "Fallback DNS lookup changed candidate priority, identity, or lifetime",
        ));
    }
    signaling
        .resolution_cache
        .insert(resolved, Instant::now())?;
    let task = tokio::spawn(signaling.run(controller.shutdown_signal()));
    let primary = async {
        let (mut socket, _address) =
            tokio::time::timeout(Duration::from_secs(5), registrar.accept())
                .await
                .map_err(|error| ControlError::Adapter(format!("Fallback backup accept: {error}")))?
                .map_err(failure)?;
        let exchanged = match &context {
            Some(context) => {
                let mut peer = context.accept(socket).await?;
                requests(&mut peer, &registrar, local, &uri, true).await
            }
            None => requests(&mut socket, &registrar, local, &uri, false).await,
        };
        exchanged
            .map_err(|error| ControlError::Adapter(format!("Fallback backup exchange: {error}")))
    }
    .await;
    controller.shutdown();
    let result = cleanup::combine(primary, finish(task).await);
    let rebound = TcpListener::bind(local)
        .await
        .map(|_listener| ())
        .map_err(failure);
    cleanup::combine(result, rebound)
}

#[tokio::test]
async fn tcp_fallback_reuses_the_open_backup_without_another_socket() -> Result<(), ControlError> {
    registration(Protocol::Tcp).await
}

#[tokio::test]
async fn tls_fallback_reuses_the_verified_backup_without_another_handshake(
) -> Result<(), ControlError> {
    registration(Protocol::Tls).await
}
