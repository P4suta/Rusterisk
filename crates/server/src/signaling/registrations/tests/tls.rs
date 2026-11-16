use super::stream::{receive, reply, Packet};
use crate::{
    encryption::TlsContext,
    media::MediaHub,
    signaling::{cleanup, Signaling},
    ControlError, Controller,
};
use rsipstack::sip::{HeadersExt, Method};
use rusterisk_config::{Config, Protocol, RegistrarUri, TlsFiles};
use rusterisk_sip::SipStreamDecoder;
use sha2::{Digest, Sha256};
use std::{net::SocketAddr, path::PathBuf, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    task::JoinHandle,
};

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}

struct Identity {
    _directory: tempfile::TempDir,
    files: TlsFiles,
    certificate: PathBuf,
}

impl Identity {
    fn new(name: &str) -> Result<Self, ControlError> {
        let directory = tempfile::tempdir().map_err(failure)?;
        let identity = rcgen::generate_simple_self_signed([name.into()]).map_err(failure)?;
        let certificate = directory.path().join("certificate.pem");
        let private_key = directory.path().join("private-key.pem");
        std::fs::write(&certificate, identity.cert.pem()).map_err(failure)?;
        std::fs::write(&private_key, identity.signing_key.serialize_pem()).map_err(failure)?;
        Ok(Self {
            files: TlsFiles {
                certificate: certificate.to_string_lossy().into(),
                private_key: private_key.to_string_lossy().into(),
            },
            certificate,
            _directory: directory,
        })
    }
}

fn configuration(
    registrar: SocketAddr,
    identity: &Identity,
    trusted: &Identity,
) -> Result<(Config, String), ControlError> {
    let uri = format!(
        "SIPS:127.0.0.1:0{};transport=TCP;extension=%61b",
        registrar.port()
    );
    let server = uri.replace(';', "\\;");
    let pjsip = format!("[native]\ntype=transport\nprotocol=tls\nbind=127.0.0.1:0\ncert_file={}\npriv_key_file={}\nca_list_file={}\n[account]\ntype=auth\nusername=account\npassword=sample\nrealm=rusterisk\n[provider]\ntype=registration\ntransport=native\noutbound_auth=account\nserver_uri={server}\nclient_uri=sips:account@provider.test\ncontact_user=line\nmax_random_initial_delay=0\n", identity.files.certificate, identity.files.private_key, trusted.certificate.display());
    let config = Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=test\n",
        &pjsip,
        "",
        "",
    )
    .map_err(failure)?;
    Ok((config, uri))
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
            "Missing TLS registration Digest attribute",
        ))
}

fn authorized(packet: &Packet, count: u32) -> Result<String, ControlError> {
    let authorization = packet
        .request
        .authorization_header()
        .ok_or(ControlError::Invalid(
            "Missing TLS registration authorization",
        ))?
        .value();
    let cnonce = quoted(authorization, "cnonce")?;
    let expected = hash(&format!(
        "{}:tls-nonce:{count:08x}:{cnonce}:auth-int:{}",
        hash("account:rusterisk:sample"),
        hash(&format!("REGISTER:{}:{}", packet.uri, hash("")))
    ));
    if !authorization.contains("algorithm=SHA-256")
        || !authorization.contains("qop=auth-int")
        || !authorization.contains(&format!("nc={count:08x}"))
        || quoted(authorization, "uri")? != packet.uri
        || quoted(authorization, "opaque")? != "tls-opaque"
        || quoted(authorization, "response")? != expected
    {
        return Err(ControlError::Invalid(
            "TLS registration changed authenticated wire octets",
        ));
    }
    Ok(cnonce.into())
}

async fn finish(
    controller: Controller,
    mut task: JoinHandle<Result<(), ControlError>>,
    primary: Result<(), ControlError>,
) -> Result<(), ControlError> {
    controller.shutdown();
    let cleanup = match tokio::time::timeout(Duration::from_secs(2), &mut task).await {
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
    cleanup::combine(primary, cleanup)
}

#[tokio::test]
async fn verified_tls_registration_authenticates_and_refreshes_on_one_owned_flow(
) -> Result<(), ControlError> {
    let identity = Identity::new("127.0.0.1")?;
    let context = TlsContext::load(&identity.files).await?;
    let registrar = TcpListener::bind("127.0.0.1:0").await.map_err(failure)?;
    let (config, uri) = configuration(
        registrar.local_addr().map_err(failure)?,
        &identity,
        &identity,
    )?;
    let (controller, _runner, effects) = Controller::new(config.clone(), 16);
    let (media, _hub, notices) = MediaHub::new(16);
    let signaling = Signaling::bind(config, controller.clone(), effects, media, notices).await?;
    let local = signaling
        .listeners()?
        .first()
        .ok_or(ControlError::Capacity)?
        .1;
    let task = tokio::spawn(signaling.run(controller.shutdown_signal()));
    let primary: Result<(), ControlError> = async {
        let (socket, _address) = tokio::time::timeout(Duration::from_secs(3), registrar.accept())
            .await.map_err(failure)?.map_err(failure)?;
        let mut peer = context.accept(socket).await?;
        let mut decoder = SipStreamDecoder::new();
        let initial = receive(&mut peer, &mut decoder).await?;
        let contact = format!("<sips:line@{local};transport=tcp>");
        if initial.uri != uri || initial.request.method != Method::Register
            || initial.request.cseq_header().map_err(failure)?.value() != "1 REGISTER"
            || initial.request.contact_header().map_err(failure)?.value() != contact
            || !initial.request.top_via_header().map_err(failure)?.value().starts_with("SIP/2.0/TLS ")
        {
            return Err(ControlError::Invalid("TLS registration changed its secure transport or URI"));
        }
        let challenge = reply(&initial.request, 401, "WWW-Authenticate: Digest realm=\"rusterisk\", nonce=\"tls-nonce\", algorithm=SHA-256, qop=\"auth-int\", opaque=\"tls-opaque\"\r\n")?;
        peer.write_all(&challenge.as_bytes()[..17]).await.map_err(failure)?;
        if tokio::time::timeout(Duration::from_millis(50), peer.read(&mut [0; 1])).await.is_ok() {
            return Err(ControlError::Invalid("TLS registration processed an incomplete challenge"));
        }
        peer.write_all(&challenge.as_bytes()[17..]).await.map_err(failure)?;
        let retry = receive(&mut peer, &mut decoder).await?;
        let cnonce = authorized(&retry, 1)?;
        if retry.uri != uri
            || retry.request.cseq_header().map_err(failure)?.value() != "2 REGISTER"
            || retry.request.call_id_header().map_err(failure)?.value() != initial.request.call_id_header().map_err(failure)?.value()
        {
            return Err(ControlError::Invalid("TLS authentication changed registration identity"));
        }
        let provisional = reply(&retry.request, 100, "")?;
        let grant = reply(&retry.request, 200, &format!("Contact: {contact};expires=11\r\nExpires: 11\r\n"))?;
        peer.write_all(format!("{provisional}{grant}").as_bytes()).await.map_err(failure)?;
        let refresh = receive(&mut peer, &mut decoder).await?;
        if refresh.uri != uri
            || refresh.request.cseq_header().map_err(failure)?.value() != "3 REGISTER"
            || refresh.request.call_id_header().map_err(failure)?.value() != initial.request.call_id_header().map_err(failure)?.value()
            || refresh.request.contact_header().map_err(failure)?.value() != contact
            || authorized(&refresh, 2)? != cnonce
        {
            return Err(ControlError::Invalid("TLS refresh changed its authorized flow"));
        }
        if tokio::time::timeout(Duration::from_millis(100), registrar.accept()).await.is_ok() {
            return Err(ControlError::Invalid("TLS registration opened another connection"));
        }
        Ok(())
    }.await;
    finish(controller, task, primary).await?;
    let _rebound = TcpListener::bind(local).await.map_err(failure)?;
    Ok(())
}

#[tokio::test]
async fn untrusted_or_name_mismatched_tls_registrars_receive_no_registration_bytes(
) -> Result<(), ControlError> {
    let matching = Identity::new("127.0.0.1")?;
    let mismatched = Identity::new("127.0.0.2")?;
    for (remote, trusted) in [(&matching, &mismatched), (&mismatched, &mismatched)] {
        let context = TlsContext::load(&remote.files).await?;
        let registrar = TcpListener::bind("127.0.0.1:0").await.map_err(failure)?;
        let (config, _uri) =
            configuration(registrar.local_addr().map_err(failure)?, &matching, trusted)?;
        let (controller, _runner, effects) = Controller::new(config.clone(), 16);
        let (media, _hub, notices) = MediaHub::new(16);
        let signaling =
            Signaling::bind(config, controller.clone(), effects, media, notices).await?;
        let local = signaling
            .listeners()?
            .first()
            .ok_or(ControlError::Capacity)?
            .1;
        let task = tokio::spawn(signaling.run(controller.shutdown_signal()));
        let primary: Result<(), ControlError> = async {
            let (socket, _address) =
                tokio::time::timeout(Duration::from_secs(3), registrar.accept())
                    .await
                    .map_err(failure)?
                    .map_err(failure)?;
            match context.accept(socket).await {
                Err(_) => Ok(()),
                Ok(mut peer) => {
                    let read = tokio::time::timeout(Duration::from_secs(1), peer.read(&mut [0; 1]))
                        .await
                        .map_err(failure)?;
                    if matches!(read, Ok(0) | Err(_)) {
                        Ok(())
                    } else {
                        Err(ControlError::Invalid(
                            "Unverified TLS registrar received SIP bytes",
                        ))
                    }
                }
            }
        }
        .await;
        finish(controller, task, primary).await?;
        let _rebound = TcpListener::bind(local).await.map_err(failure)?;
    }
    Ok(())
}

#[test]
fn tls_registrar_serialization_uses_the_secure_default_port() -> Result<(), ControlError> {
    let identity = Identity::new("127.0.0.1")?;
    for (other, same) in [
        ("sips:PROVIDER.test.:5061", true),
        ("sips:provider.test:5060", false),
        ("sips:other.test:5061", false),
    ] {
        let (mut config, _uri) = configuration(
            "127.0.0.1:5071".parse().map_err(failure)?,
            &identity,
            &identity,
        )?;
        let first = config
            .registrations
            .first_mut()
            .ok_or(ControlError::Capacity)?;
        first.server_uri =
            RegistrarUri::parse(Protocol::Tls, "sips:provider.test").map_err(failure)?;
        let mut second = first.clone();
        second.name = "explicit-port".into();
        second.server_uri = RegistrarUri::parse(Protocol::Tls, other).map_err(failure)?;
        config.registrations.push(second);
        let manager = super::Registrations::new(&config)?;
        assert_eq!(manager.clients[0].same_registrar(&manager.clients[1]), same);
    }
    Ok(())
}
