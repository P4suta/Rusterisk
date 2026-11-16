use super::{check, failure};
use crate::{
    encryption::TlsContext,
    media::MediaHub,
    signaling::{
        cleanup,
        registrations::tests::stream::{receive as receive_packet, reply, Packet},
        Signaling,
    },
    ControlError, Controller,
};
use rsipstack::sip::HeadersExt;
use rusterisk_config::{Config, Protocol, TlsFiles};
use rusterisk_sip::SipStreamDecoder;
use std::{net::SocketAddr, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncWrite, AsyncWriteExt},
    net::TcpListener,
    task::JoinHandle,
};

struct Identity {
    _directory: tempfile::TempDir,
    files: TlsFiles,
}

impl Identity {
    fn new() -> Result<Self, ControlError> {
        let directory = tempfile::tempdir().map_err(failure)?;
        let identity = rcgen::generate_simple_self_signed(["127.0.0.1".into()]).map_err(failure)?;
        let certificate = directory.path().join("certificate.pem");
        let private_key = directory.path().join("private-key.pem");
        std::fs::write(&certificate, identity.cert.pem()).map_err(failure)?;
        std::fs::write(&private_key, identity.signing_key.serialize_pem()).map_err(failure)?;
        Ok(Self {
            files: TlsFiles {
                certificate: certificate.to_string_lossy().into(),
                private_key: private_key.to_string_lossy().into(),
            },
            _directory: directory,
        })
    }
}

fn configuration(
    protocol: Protocol,
    remote: SocketAddr,
    identity: Option<&Identity>,
) -> Result<(Config, String), ControlError> {
    let (token, scheme) = match protocol {
        Protocol::Tcp => ("tcp", "sip"),
        Protocol::Tls => ("tls", "sips"),
        Protocol::Udp | Protocol::Ws | Protocol::Wss => return Err(ControlError::Unsupported),
    };
    let files = identity
        .map(|identity| {
            format!(
                "cert_file={}\npriv_key_file={}\nca_list_file={}\n",
                identity.files.certificate, identity.files.private_key, identity.files.certificate
            )
        })
        .unwrap_or_default();
    let uri = format!(
        "{}:127.0.0.1:0{};transport=TCP;extension=%61b",
        scheme.to_ascii_uppercase(),
        remote.port()
    );
    let escaped = uri.replace(';', "\\;");
    let config = Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=test\n",
        &format!("[native]\ntype=transport\nprotocol={token}\nbind=127.0.0.1:0\n{files}[account]\ntype=auth\nusername=account\npassword=sample\nrealm=rusterisk\n[provider]\ntype=registration\ntransport=native\noutbound_auth=account\nserver_uri={escaped}\nclient_uri={scheme}:account@provider.test\ncontact_user=line\nexpiration=1\nmax_random_initial_delay=0\n"),
        "",
        "",
    ).map_err(failure)?;
    Ok((config, uri))
}

async fn receive<S: AsyncRead + Unpin>(
    peer: &mut S,
    decoder: &mut SipStreamDecoder,
    uri: &str,
) -> Result<Packet, ControlError> {
    let packet = receive_packet(peer, decoder).await?;
    if packet.uri != uri {
        return Err(ControlError::Invalid(
            "Expiration correction changed Request-URI octets",
        ));
    }
    Ok(packet)
}

async fn requests<S: AsyncRead + AsyncWrite + Unpin>(
    peer: &mut S,
    uri: &str,
    contact: &str,
) -> Result<(), ControlError> {
    let mut decoder = SipStreamDecoder::new();
    let initial = receive(peer, &mut decoder, uri).await?;
    if initial.request.contact_header().map_err(failure)?.value() != contact {
        return Err(ControlError::Invalid(
            "Stream correction changed its initial URI or Contact",
        ));
    }
    peer.write_all(reply(&initial.request, 401, "WWW-Authenticate: Digest realm=\"rusterisk\", nonce=\"minimum-nonce\", algorithm=SHA-256, qop=\"auth-int\"\r\n")?.as_bytes()).await.map_err(failure)?;
    let authenticated = receive(peer, &mut decoder, uri).await?;
    check(&authenticated.request, &initial.request, uri, 1, 2, 1)?;
    peer.write_all(reply(&authenticated.request, 423, "Min-Expires: 11\r\n")?.as_bytes())
        .await
        .map_err(failure)?;
    let corrected = receive(peer, &mut decoder, uri).await?;
    check(&corrected.request, &initial.request, uri, 11, 3, 2)?;
    peer.write_all(
        reply(
            &corrected.request,
            200,
            &format!("Contact: {contact};expires=11\r\n"),
        )?
        .as_bytes(),
    )
    .await
    .map_err(failure)?;
    let refreshed = receive(peer, &mut decoder, uri).await?;
    check(&refreshed.request, &initial.request, uri, 11, 4, 3)
}

async fn finish(
    controller: Controller,
    mut task: JoinHandle<Result<(), ControlError>>,
    primary: Result<(), ControlError>,
) -> Result<(), ControlError> {
    controller.shutdown();
    let result = match tokio::time::timeout(Duration::from_secs(2), &mut task).await {
        Ok(result) => result.map_err(failure).and_then(|result| result),
        Err(timeout) => {
            task.abort();
            let joined = match task.await {
                Err(error) if error.is_cancelled() => Ok(()),
                Err(error) => Err(failure(error)),
                Ok(result) => result,
            };
            cleanup::combine(Err(failure(timeout)), joined)
        }
    };
    cleanup::combine(primary, result)
}

async fn registration(protocol: Protocol) -> Result<(), ControlError> {
    let registrar = TcpListener::bind("127.0.0.1:0").await.map_err(failure)?;
    let identity = (protocol == Protocol::Tls)
        .then(Identity::new)
        .transpose()?;
    let (config, uri) = configuration(
        protocol,
        registrar.local_addr().map_err(failure)?,
        identity.as_ref(),
    )?;
    let context = match &identity {
        Some(identity) => Some(TlsContext::load(&identity.files).await?),
        None => None,
    };
    let (controller, _runner, effects) = Controller::new(config.clone(), 16);
    let (media, _hub, notices) = MediaHub::new(16);
    let signaling = Signaling::bind(config, controller.clone(), effects, media, notices).await?;
    let local = signaling
        .listeners()?
        .first()
        .ok_or(ControlError::Capacity)?
        .1;
    let scheme = if protocol == Protocol::Tls {
        "sips"
    } else {
        "sip"
    };
    let contact = format!("<{scheme}:line@{local};transport=tcp>");
    let task = tokio::spawn(signaling.run(controller.shutdown_signal()));
    let primary = async {
        let (mut socket, _peer) = tokio::time::timeout(Duration::from_secs(3), registrar.accept()).await.map_err(failure)?.map_err(failure)?;
        let exchange = async {
            match &context {
                Some(context) => {
                    let mut peer = context.accept(socket).await?;
                    requests(&mut peer, &uri, &contact).await
                }
                None => requests(&mut socket, &uri, &contact).await,
            }
        };
        tokio::select! {
            biased;
            extra = registrar.accept() => match extra {
                Ok((_socket, _peer)) => Err(ControlError::Invalid("Expiration correction opened another signaling flow")),
                Err(error) => Err(failure(error)),
            },
            result = exchange => result,
        }
    }.await;
    finish(controller, task, primary).await?;
    let _rebound = TcpListener::bind(local).await.map_err(failure)?;
    Ok(())
}

#[tokio::test]
async fn tcp_correction_and_refresh_keep_the_authenticated_connection() -> Result<(), ControlError>
{
    registration(Protocol::Tcp).await
}

#[tokio::test]
async fn tls_correction_and_refresh_keep_the_verified_connection() -> Result<(), ControlError> {
    registration(Protocol::Tls).await
}
