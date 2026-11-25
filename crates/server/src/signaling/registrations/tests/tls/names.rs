use super::{authorized, configuration, failure, finish, receive, reply, Identity};
use crate::{
    encryption::TlsContext,
    media::MediaHub,
    signaling::{registrations::tests::dns, Signaling},
    ControlError, Controller,
};
use hickory_resolver::proto::{
    op::{Message, OpCode},
    rr::{
        rdata::{A, SRV},
        Name, RData, Record, RecordType,
    },
};
use rsipstack::sip::HeadersExt;
use rusterisk_config::{Protocol, RegistrarUri, TlsFiles};
use rusterisk_sip::SipStreamDecoder;
use rusterisk_test_support::Wait;
use std::net::Ipv4Addr;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, UdpSocket},
};

fn domain_identity(domain: &str) -> Result<Identity, ControlError> {
    let directory = tempfile::tempdir().map_err(failure)?;
    let mut params = rcgen::CertificateParams::default();
    params.subject_alt_names = std::iter::once(rcgen::SanType::URI(
        format!("sip:{domain}").try_into().map_err(failure)?,
    ))
    .collect();
    let key = rcgen::KeyPair::generate().map_err(failure)?;
    let certificate = params.self_signed(&key).map_err(failure)?;
    let certificate_path = directory.path().join("certificate.pem");
    let private_key = directory.path().join("private-key.pem");
    std::fs::write(&certificate_path, certificate.pem()).map_err(failure)?;
    std::fs::write(&private_key, key.serialize_pem()).map_err(failure)?;
    Ok(Identity {
        _directory: directory,
        files: TlsFiles {
            certificate: certificate_path.to_string_lossy().into(),
            private_key: private_key.to_string_lossy().into(),
        },
        certificate: certificate_path,
    })
}

async fn answer(
    socket: &UdpSocket,
    name: &str,
    kind: RecordType,
    record: RData,
) -> Result<(), ControlError> {
    let (bytes, peer) = dns::receive(socket).await?;
    let query = Message::from_vec(&bytes).map_err(failure)?;
    let question = query.queries.first().ok_or(ControlError::Capacity)?.clone();
    if query.queries.len() != 1
        || !question.name().to_ascii().eq_ignore_ascii_case(name)
        || question.query_type() != kind
    {
        return Err(ControlError::Invalid(
            "Unexpected TLS registration DNS query",
        ));
    }
    let mut response = Message::response(query.metadata.id, OpCode::Query);
    response.metadata.authoritative = true;
    response.metadata.recursion_available = true;
    response.metadata.recursion_desired = query.metadata.recursion_desired;
    response.queries.push(question.clone());
    response
        .answers
        .push(Record::from_rdata(question.name().clone(), 60, record));
    response.edns = query.edns;
    dns::send(socket, &response.to_vec().map_err(failure)?, peer).await
}

async fn registration(identity: Identity, accepted: bool) -> Result<(), ControlError> {
    let context = TlsContext::load(&identity.files).await?;
    let dns = UdpSocket::bind("127.0.0.1:0").await.map_err(failure)?;
    let registrar = TcpListener::bind("127.0.0.1:0").await.map_err(failure)?;
    let remote = registrar.local_addr().map_err(failure)?;
    let (mut config, _numeric_uri) = configuration(remote, &identity, &identity)?;
    let uri = "SIPS:PrOvIdEr.TeSt;transport=TCP;extension=%61b";
    config
        .registrations
        .first_mut()
        .ok_or(ControlError::Capacity)?
        .server_uri = RegistrarUri::parse(Protocol::Tls, uri).map_err(failure)?;
    let (controller, runner, _effects) = Controller::new(config.clone(), 16);
    let (media, _hub, notices) = MediaHub::new(16);
    let mut signaling = Signaling::bind(config, controller.clone(), runner, media, notices).await?;
    signaling.resolver = Ok(dns::resolver(&dns)?);
    let local = signaling
        .listeners()?
        .first()
        .ok_or(ControlError::Capacity)?
        .1;
    let task = tokio::spawn(signaling.run(controller.shutdown_signal()));
    let primary: Result<(), ControlError> = async {
        answer(
            &dns,
            "_sips._tcp.provider.test.",
            RecordType::SRV,
            RData::SRV(SRV::new(0, 1, remote.port(), Name::from_ascii("edge.test.").map_err(failure)?)),
        ).await?;
        answer(&dns, "edge.test.", RecordType::A, RData::A(A(Ipv4Addr::LOCALHOST))).await?;
        let (socket, _address) = Wait::Ready.bounded(registrar.accept())
            .await.map_err(failure)?.map_err(failure)?;
        let established = context.accept(socket).await;
        if !accepted {
            return match established {
                Err(_) => Ok(()),
                Ok(mut peer) => {
                    let read = Wait::Ready.bounded(peer.read(&mut [0; 1]))
                        .await.map_err(failure)?;
                    if matches!(read, Ok(0) | Err(_)) {
                        Ok(())
                    } else {
                        Err(ControlError::Invalid("Resolved host replaced the TLS registration identity"))
                    }
                }
            };
        }
        let mut peer = established?;
        if !peer.get_ref().1.server_name().is_some_and(|name| name.eq_ignore_ascii_case("provider.test")) {
            return Err(ControlError::Invalid("TLS registration SNI lost the original domain"));
        }
        let mut decoder = SipStreamDecoder::new();
        let initial = receive(&mut peer, &mut decoder).await?;
        if initial.uri != uri {
            return Err(ControlError::Invalid("TLS service lookup changed Request-URI octets"));
        }
        let challenge = reply(&initial.request, 401, "WWW-Authenticate: Digest realm=\"rusterisk\", nonce=\"tls-nonce\", algorithm=SHA-256, qop=\"auth-int\", opaque=\"tls-opaque\"\r\n")?;
        peer.write_all(challenge.as_bytes()).await.map_err(failure)?;
        let retry = receive(&mut peer, &mut decoder).await?;
        let cnonce = authorized(&retry, 1)?;
        let contact = initial.request.contact_header().map_err(failure)?.value();
        if retry.uri != uri
            || retry.request.cseq_header().map_err(failure)?.value() != "2 REGISTER"
            || retry.request.call_id_header().map_err(failure)?.value() != initial.request.call_id_header().map_err(failure)?.value()
        {
            return Err(ControlError::Invalid("Resolved TLS authentication changed registration identity"));
        }
        let grant = reply(&retry.request, 200, &format!("Contact: {contact};expires=11\r\nExpires: 11\r\n"))?;
        peer.write_all(grant.as_bytes()).await.map_err(failure)?;
        let refresh = receive(&mut peer, &mut decoder).await?;
        if refresh.uri != uri
            || refresh.request.cseq_header().map_err(failure)?.value() != "3 REGISTER"
            || authorized(&refresh, 2)? != cnonce
        {
            return Err(ControlError::Invalid("Resolved TLS refresh changed the original domain"));
        }
        if Wait::Quiet.bounded(registrar.accept()).await.is_ok() {
            return Err(ControlError::Invalid("Resolved TLS registration opened another flow"));
        }
        Ok(())
    }.await;
    finish(controller, task, primary).await?;
    let _rebound = TcpListener::bind(local).await.map_err(failure)?;
    Ok(())
}

#[tokio::test]
async fn tls_registration_follows_srv_and_authenticates_the_original_uri_domain(
) -> Result<(), ControlError> {
    registration(domain_identity("provider.test")?, true).await
}

#[tokio::test]
async fn srv_target_and_resolved_ip_certificates_cannot_replace_the_registration_domain(
) -> Result<(), ControlError> {
    for identity in [domain_identity("edge.test")?, Identity::new("127.0.0.1")?] {
        registration(identity, false).await?;
    }
    Ok(())
}
