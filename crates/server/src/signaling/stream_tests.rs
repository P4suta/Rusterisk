use super::{failure, Signaling};
use crate::{
    encryption::{TlsClientContext, TlsContext},
    media::MediaHub,
    Action, ControlError, Controller, Credentials,
};
use rsipstack::sip::{
    headers::{
        auth::{Algorithm, AuthQop},
        typed::WwwAuthenticate,
    },
    services::DigestGenerator,
    HeadersExt, Method, Request, SipMessage, Uri,
};
use rusterisk_api::{Channel, ChannelId, ChannelState};
use rusterisk_config::{Codec, Config, TlsFiles};
use rusterisk_sip::{Session, SipStreamDecoder, StreamItem};
use rusterisk_test_support::Wait;
use std::net::SocketAddr;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::TcpStream,
};
use tokio_rustls::rustls::pki_types::ServerName;
mod domain_verification;
mod starting_profile;

fn configuration(protocol: &str, files: &str) -> Result<Config, ControlError> {
    Config::parse("[general]\nbindaddr=127.0.0.1\n", "[app]\ntype=user\npassword=test-password\n", &format!("[stream]\ntype=transport\nprotocol={protocol}\nbind=127.0.0.1:0\n{files}[auth]\ntype=auth\nusername=phone\npassword=test-password\n[phone]\ntype=aor\nmax_contacts=1\n[phone]\ntype=endpoint\ntransport=stream\nauth=auth\naors=phone\ncontext=internal\nallow=ulaw\n"), "", "[internal]\nexten => 100,1,Stasis(calls)\n").map_err(failure)
}

async fn receive<S: AsyncRead + Unpin>(
    peer: &mut S,
    decoder: &mut SipStreamDecoder,
) -> Result<SipMessage, ControlError> {
    Wait::Ready
        .bounded(async {
            let mut byte = [0; 1];
            loop {
                if peer.read(&mut byte).await.map_err(failure)? == 0 {
                    return Err(ControlError::Invalid("Signaling peer closed"));
                }
                match decoder.push(&byte).map_err(failure)?.1 {
                    Some(StreamItem::Message(bytes)) => {
                        return SipMessage::try_from(bytes.as_slice()).map_err(failure);
                    }
                    Some(StreamItem::Crlf) | None => {}
                }
            }
        })
        .await
        .map_err(failure)?
}

fn registration(
    local: SocketAddr,
    target: SocketAddr,
    token: &str,
    serial: u32,
    authorization: &str,
) -> String {
    format!(
        "REGISTER sip:{target} SIP/2.0\r\nVia: SIP/2.0/{token} {local};branch=z9hG4bK-register-{serial};rport\r\nFrom: <sip:phone@localhost>;tag=phone\r\nTo: <sip:phone@localhost>\r\nCall-ID: registration\r\nCSeq: {serial} REGISTER\r\nContact: <sip:phone@{local};transport=tcp>\r\nExpires: 120\r\nMax-Forwards: 70\r\n{authorization}Content-Length: 0\r\n\r\n"
    )
}

async fn registered<S: AsyncRead + AsyncWrite + Unpin>(
    peer: &mut S,
    local: SocketAddr,
    target: SocketAddr,
    token: &str,
    decoder: &mut SipStreamDecoder,
) -> Result<(), ControlError> {
    let initial = registration(local, target, token, 1, "");
    peer.write_all(&initial.as_bytes()[..17])
        .await
        .map_err(failure)?;
    let mut byte = [0; 1];
    assert!(Wait::Quiet.bounded(peer.read(&mut byte)).await.is_err());
    peer.write_all(&initial.as_bytes()[17..])
        .await
        .map_err(failure)?;
    let SipMessage::Response(challenge) = receive(peer, decoder).await? else {
        return Err(ControlError::Unsupported);
    };
    assert_eq!(challenge.status_code.code(), 401);
    let challenge = WwwAuthenticate::parse(
        challenge
            .www_authenticate_header()
            .ok_or(ControlError::Capacity)?
            .value(),
    )
    .map_err(failure)?;
    let uri = Uri::try_from(format!("sip:{target}")).map_err(failure)?;
    let qop = AuthQop::Auth {
        cnonce: "native-client".into(),
        nc: 1,
    };
    let digest = DigestGenerator {
        username: "phone",
        password: "test-password",
        nonce: &challenge.nonce,
        uri: &uri,
        realm: &challenge.realm,
        method: &Method::Register,
        qop: Some(&qop),
        algorithm: Algorithm::Md5,
    }
    .compute();
    let authorization = format!(
        "Authorization: Digest username=\"phone\", realm=\"{}\", nonce=\"{}\", uri=\"{uri}\", response=\"{digest}\", algorithm=MD5, qop=auth, cnonce=\"native-client\", nc=00000001\r\n",
        challenge.realm, challenge.nonce
    );
    peer.write_all(registration(local, target, token, 2, &authorization).as_bytes())
        .await
        .map_err(failure)?;
    let SipMessage::Response(response) = receive(peer, decoder).await? else {
        return Err(ControlError::Unsupported);
    };
    assert_eq!(response.status_code.code(), 200);
    Ok(())
}

fn credentials() -> Result<Credentials, ControlError> {
    Credentials::new("app".into(), "test-password".into())
}

async fn channel_state(
    controller: &Controller,
    id: &ChannelId,
    expected: Option<ChannelState>,
) -> Result<(), ControlError> {
    Wait::Ready
        .bounded(async {
            loop {
                match controller
                    .execute(credentials()?, None, Action::GetChannel(id.clone()))
                    .await
                {
                    Ok(response) => {
                        let channel: Channel =
                            serde_json::from_value(response.body.ok_or(ControlError::Capacity)?)?;
                        if Some(channel.state) == expected {
                            return Ok::<(), ControlError>(());
                        }
                    }
                    Err(ControlError::NotFound(_)) if expected.is_none() => return Ok(()),
                    Err(error) => return Err(error),
                }
            }
        })
        .await
        .map_err(failure)?
}

fn answer(request: &Request, local: SocketAddr) -> Result<String, ControlError> {
    let body = Session::parse(
        std::str::from_utf8(&request.body).map_err(failure)?,
        &[Codec::Pcmu],
    )
    .map_err(failure)?
    .answer("127.0.0.1:19000".parse().map_err(failure)?, 1)
    .map_err(failure)?;
    Ok(format!(
        "SIP/2.0 200 OK\r\nVia: {}\r\nFrom: {}\r\nTo: {};tag=phone\r\nCall-ID: {}\r\nCSeq: {}\r\nContact: <sip:phone@{local};transport=tcp>\r\nContent-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{body}",
        request.top_via_header().map_err(failure)?.value(),
        request.from_header().map_err(failure)?.value(),
        request.to_header().map_err(failure)?.value(),
        request.call_id_header().map_err(failure)?.value(),
        request.cseq_header().map_err(failure)?.value(),
        body.len()
    ))
}

async fn originated<S: AsyncRead + AsyncWrite + Unpin>(
    peer: &mut S,
    decoder: &mut SipStreamDecoder,
    controller: &Controller,
    id: &str,
    token: &str,
) -> Result<(ChannelId, Request), ControlError> {
    let id = ChannelId::try_from(id.to_owned())?;
    controller
        .execute(
            credentials()?,
            None,
            Action::Originate {
                id: id.clone(),
                endpoint: "PJSIP/phone".into(),
                app: "app".into(),
                args: Vec::new(),
                caller: "100".into(),
                timeout: 30,
            },
        )
        .await?;
    let SipMessage::Request(request) = receive(peer, decoder).await? else {
        return Err(ControlError::Unsupported);
    };
    assert_eq!(request.method, Method::Invite);
    assert!(request
        .top_via_header()
        .map_err(failure)?
        .value()
        .starts_with(&format!("SIP/2.0/{token} ")));
    let mut byte = [0; 1];
    assert!(Wait::Responsive
        .bounded(peer.read(&mut byte))
        .await
        .is_err());
    Ok((id, request))
}

async fn exercise<S: AsyncRead + AsyncWrite + Unpin>(
    mut peer: S,
    local: SocketAddr,
    target: SocketAddr,
    token: &str,
    controller: &Controller,
) -> Result<(), ControlError> {
    let mut decoder = SipStreamDecoder::new();
    let subscription = controller
        .subscribe(credentials()?, None, "app".into())
        .await?;
    registered(&mut peer, local, target, token, &mut decoder).await?;
    let options = |sequence| {
        format!(
            "OPTIONS sip:{target} SIP/2.0\r\nVia: SIP/2.0/{token} {local};branch=z9hG4bK-options-{sequence}\r\nFrom: <sip:phone@localhost>;tag=phone\r\nTo: <sip:server@localhost>\r\nCall-ID: options\r\nCSeq: {sequence} OPTIONS\r\nMax-Forwards: 70\r\nContent-Length: 0\r\n\r\n"
        )
    };
    peer.write_all(format!("{}{}", options(1), options(2)).as_bytes())
        .await
        .map_err(failure)?;
    for sequence in [1, 2] {
        let SipMessage::Response(response) = receive(&mut peer, &mut decoder).await? else {
            return Err(ControlError::Unsupported);
        };
        assert_eq!(response.status_code.code(), 200);
        assert_eq!(
            response.cseq_header().map_err(failure)?.value(),
            format!("{sequence} OPTIONS")
        );
    }
    let (id, invite) =
        originated(&mut peer, &mut decoder, controller, "native-call", token).await?;
    let response = answer(&invite, local)?;
    for bytes in response.as_bytes().chunks(13) {
        peer.write_all(bytes).await.map_err(failure)?;
    }
    let SipMessage::Request(ack) = receive(&mut peer, &mut decoder).await? else {
        return Err(ControlError::Unsupported);
    };
    assert_eq!(ack.method, Method::Ack);
    channel_state(controller, &id, Some(ChannelState::Up)).await?;
    controller
        .execute(credentials()?, None, Action::Hangup(id.clone()))
        .await?;
    let SipMessage::Request(bye) = receive(&mut peer, &mut decoder).await? else {
        return Err(ControlError::Unsupported);
    };
    assert_eq!(bye.method, Method::Bye);
    assert!(bye
        .top_via_header()
        .map_err(failure)?
        .value()
        .starts_with(&format!("SIP/2.0/{token} ")));
    let response = format!(
        "SIP/2.0 200 OK\r\nVia: {}\r\nFrom: {}\r\nTo: {}\r\nCall-ID: {}\r\nCSeq: {}\r\nContent-Length: 0\r\n\r\n",
        bye.top_via_header().map_err(failure)?.value(),
        bye.from_header().map_err(failure)?.value(),
        bye.to_header().map_err(failure)?.value(),
        bye.call_id_header().map_err(failure)?.value(),
        bye.cseq_header().map_err(failure)?.value()
    );
    peer.write_all(response.as_bytes()).await.map_err(failure)?;
    channel_state(controller, &id, None).await?;
    let (id, _) = originated(
        &mut peer,
        &mut decoder,
        controller,
        "disconnected-call",
        token,
    )
    .await?;
    drop(peer);
    channel_state(controller, &id, None).await?;
    drop(subscription);
    Ok(())
}

#[tokio::test]
async fn native_tcp_and_tls_reuse_registered_flows_for_calls_and_release_disconnected_calls(
) -> Result<(), ControlError> {
    let directory = tempfile::tempdir().map_err(failure)?;
    let identity = rcgen::generate_simple_self_signed(["localhost".into()]).map_err(failure)?;
    let certificate = directory.path().join("certificate.pem");
    let private_key = directory.path().join("private-key.pem");
    std::fs::write(&certificate, identity.cert.pem()).map_err(failure)?;
    std::fs::write(&private_key, identity.signing_key.serialize_pem()).map_err(failure)?;
    for (protocol, token) in [("tcp", "TCP"), ("tls", "TLS")] {
        let files = if protocol == "tls" {
            format!(
                "cert_file={}\npriv_key_file={}\nca_list_file={}\n",
                certificate.display(),
                private_key.display(),
                certificate.display()
            )
        } else {
            String::new()
        };
        let config = configuration(protocol, &files)?;
        let (controller, runner, _effects) = Controller::new(config.clone(), 4);
        let (media, hub, notices) = MediaHub::new(4);
        let signaling = Signaling::bind(config, controller.clone(), runner, media, notices).await?;
        let address = signaling.listeners()?[0].1;
        let hub = tokio::spawn(hub.run(controller.shutdown_signal()));
        let signaling = tokio::spawn(signaling.run(controller.shutdown_signal()));
        let result = async {
            let peer = TcpStream::connect(address).await.map_err(failure)?;
            let local = peer.local_addr().map_err(failure)?;
            match protocol {
                "tcp" => exercise(peer, local, address, token, &controller).await,
                "tls" => {
                    let client =
                        TlsClientContext::load(Some(certificate.to_string_lossy().as_ref()))?;
                    let peer = client
                        .connect(peer, ServerName::try_from("localhost").map_err(failure)?)
                        .await?;
                    exercise(peer, local, address, token, &controller).await
                }
                _ => Err(ControlError::Unsupported),
            }
        }
        .await;
        controller.shutdown();
        signaling.await.map_err(failure)??;
        hub.await.map_err(failure)?.map_err(failure)?;
        let listener = tokio::net::TcpListener::bind(address)
            .await
            .map_err(failure)?;
        assert_eq!(listener.local_addr().map_err(failure)?, address);
        result?;
    }
    Ok(())
}

async fn initial_call<S: AsyncRead + AsyncWrite + Unpin>(
    mut peer: S,
    local: SocketAddr,
    token: &str,
    controller: &Controller,
    id: &ChannelId,
    target: &str,
) -> Result<(), ControlError> {
    let mut decoder = SipStreamDecoder::new();
    let SipMessage::Request(invite) = receive(&mut peer, &mut decoder).await? else {
        return Err(ControlError::Unsupported);
    };
    assert_eq!(invite.method, Method::Invite);
    assert_eq!(invite.uri, Uri::try_from(target).map_err(failure)?);
    assert!(invite
        .top_via_header()
        .map_err(failure)?
        .value()
        .starts_with(&format!("SIP/2.0/{token} ")));
    peer.write_all(answer(&invite, local)?.as_bytes())
        .await
        .map_err(failure)?;
    let SipMessage::Request(ack) = receive(&mut peer, &mut decoder).await? else {
        return Err(ControlError::Unsupported);
    };
    assert_eq!(ack.method, Method::Ack);
    channel_state(controller, id, Some(ChannelState::Up)).await?;
    drop(peer);
    channel_state(controller, id, None).await
}

#[tokio::test]
async fn initial_outbound_connections_use_tcp_or_verified_tls_and_fail_closed(
) -> Result<(), ControlError> {
    let directory = tempfile::tempdir().map_err(failure)?;
    let identity = rcgen::generate_simple_self_signed(["127.0.0.1".into()]).map_err(failure)?;
    let wrong_name = rcgen::generate_simple_self_signed(["localhost".into()]).map_err(failure)?;
    let certificate = directory.path().join("certificate.pem");
    let private_key = directory.path().join("private-key.pem");
    let other_certificate = directory.path().join("other-certificate.pem");
    let other_key = directory.path().join("other-key.pem");
    std::fs::write(&certificate, identity.cert.pem()).map_err(failure)?;
    std::fs::write(&private_key, identity.signing_key.serialize_pem()).map_err(failure)?;
    std::fs::write(&other_certificate, wrong_name.cert.pem()).map_err(failure)?;
    std::fs::write(&other_key, wrong_name.signing_key.serialize_pem()).map_err(failure)?;
    for (protocol, token, name_matches, trusted, hostname) in [
        ("tcp", "TCP", true, true, false),
        ("tcp", "TCP", true, true, true),
        ("tls", "TLS", true, true, false),
        ("tls", "TLS", true, true, true),
        ("tls", "TLS", false, true, false),
        ("tls", "TLS", true, false, false),
    ] {
        let peer_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(failure)?;
        let peer_address = peer_listener.local_addr().map_err(failure)?;
        let remote_certificate = if name_matches != hostname {
            &certificate
        } else {
            &other_certificate
        };
        let remote_key = if name_matches != hostname {
            &private_key
        } else {
            &other_key
        };
        let ca = if trusted {
            remote_certificate
        } else {
            &other_certificate
        };
        let files = if protocol == "tls" {
            format!(
                "cert_file={}\npriv_key_file={}\nca_list_file={}\n",
                certificate.display(),
                private_key.display(),
                ca.display()
            )
        } else {
            String::new()
        };
        let mut config = configuration(protocol, &files)?;
        let target = if hostname {
            format!("sip:phone@localhost:{}", peer_address.port())
        } else {
            format!("sip:phone@{peer_address}")
        };
        config.aors[0].contact = Some(target.clone());
        let (controller, runner, _effects) = Controller::new(config.clone(), 4);
        let (media, hub, notices) = MediaHub::new(4);
        let signaling = Signaling::bind(config, controller.clone(), runner, media, notices).await?;
        let hub = tokio::spawn(hub.run(controller.shutdown_signal()));
        let signaling = tokio::spawn(signaling.run(controller.shutdown_signal()));
        let result = async {
            let subscription = controller
                .subscribe(credentials()?, None, "app".into())
                .await?;
            let id = ChannelId::try_from("initial-call".to_owned())?;
            controller
                .execute(
                    credentials()?,
                    None,
                    Action::Originate {
                        id: id.clone(),
                        endpoint: "PJSIP/phone".into(),
                        app: "app".into(),
                        args: Vec::new(),
                        caller: "100".into(),
                        timeout: 30,
                    },
                )
                .await?;
            let (peer, _) = Wait::Ready
                .bounded(peer_listener.accept())
                .await
                .map_err(failure)?
                .map_err(failure)?;
            match protocol {
                "tcp" => initial_call(peer, peer_address, token, &controller, &id, &target).await?,
                "tls" => {
                    let context = TlsContext::load(&TlsFiles {
                        certificate: remote_certificate.to_string_lossy().into(),
                        private_key: remote_key.to_string_lossy().into(),
                    })
                    .await?;
                    let accepted = context.accept(peer).await;
                    if name_matches && trusted {
                        initial_call(accepted?, peer_address, token, &controller, &id, &target)
                            .await?;
                    } else {
                        if let Ok(mut peer) = accepted {
                            let mut byte = [0; 1];
                            let read = Wait::Ready
                                .bounded(peer.read(&mut byte))
                                .await
                                .map_err(failure)?;
                            assert!(matches!(read, Ok(0) | Err(_)));
                        }
                        channel_state(&controller, &id, None).await?;
                    }
                }
                _ => return Err(ControlError::Unsupported),
            }
            drop(subscription);
            Ok(())
        }
        .await;
        controller.shutdown();
        signaling.await.map_err(failure)??;
        hub.await.map_err(failure)?.map_err(failure)?;
        result?;
    }
    Ok(())
}
