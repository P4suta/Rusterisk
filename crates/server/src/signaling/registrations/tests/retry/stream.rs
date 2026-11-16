use crate::{
    encryption::TlsContext,
    media::MediaHub,
    signaling::{
        cleanup,
        registrations::{
            tests::{minimum_expiration::check, stream, tls},
            State,
        },
        transport::{ConnectionKey, Input},
        Signaling,
    },
    ControlError, Controller,
};
use rsipstack::sip::{HeadersExt, Request};
use rusterisk_config::Protocol;
use rusterisk_sip::SipStreamDecoder;
use std::{future::Future, net::SocketAddr, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::TcpListener,
};

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}

#[derive(Clone, Copy)]
enum Scenario {
    Retry,
    Cancel,
    Disconnect,
}

struct Fixture {
    signaling: Signaling,
    registrar: TcpListener,
    local: SocketAddr,
    uri: String,
    context: Option<TlsContext>,
    _identity: Option<tls::Identity>,
}

impl Fixture {
    async fn new(protocol: Protocol) -> Result<Self, ControlError> {
        let registrar = TcpListener::bind("127.0.0.1:0").await.map_err(failure)?;
        let remote = registrar.local_addr().map_err(failure)?;
        let identity = (protocol == Protocol::Tls)
            .then(|| tls::Identity::new("127.0.0.1"))
            .transpose()?;
        let (mut config, uri) = match &identity {
            Some(identity) => tls::configuration(remote, identity, identity)?,
            None => {
                let uri = format!(
                    "SIP:127.0.0.1:0{};transport=TCP;extension=%61b",
                    remote.port()
                );
                (stream::configuration(&uri)?, uri)
            }
        };
        let settings = config
            .registrations
            .first_mut()
            .ok_or(ControlError::Capacity)?;
        settings.expiration = 11;
        settings.retry_interval = 2;
        settings.max_retries = 1;
        let context = match &identity {
            Some(identity) => Some(TlsContext::load(&identity.files).await?),
            None => None,
        };
        let (controller, _runner, effects) = Controller::new(config.clone(), 16);
        let (media, _hub, notices) = MediaHub::new(16);
        let signaling = Signaling::bind(config, controller, effects, media, notices).await?;
        let local = signaling
            .listeners()?
            .first()
            .ok_or(ControlError::Capacity)?
            .1;
        Ok(Self {
            signaling,
            registrar,
            local,
            uri,
            context,
            _identity: identity,
        })
    }

    async fn exchange(
        &mut self,
        scenario: Scenario,
    ) -> Result<Option<(ConnectionKey, Request)>, ControlError> {
        let socket = async {
            let (socket, _address) = self.registrar.accept().await.map_err(failure)?;
            Ok(socket)
        };
        match &self.context {
            Some(context) => {
                let mut peer = prepare(
                    &mut self.signaling,
                    async { context.accept(socket.await?).await },
                    1000,
                )
                .await?;
                exchange(
                    &mut self.signaling,
                    &mut peer,
                    &self.registrar,
                    &self.uri,
                    scenario,
                )
                .await
            }
            None => {
                let mut peer = prepare(&mut self.signaling, socket, 1000).await?;
                exchange(
                    &mut self.signaling,
                    &mut peer,
                    &self.registrar,
                    &self.uri,
                    scenario,
                )
                .await
            }
        }
    }

    async fn reconnect(
        &mut self,
        old: ConnectionKey,
        initial: &Request,
    ) -> Result<(), ControlError> {
        let socket = async {
            let (socket, _address) = self.registrar.accept().await.map_err(failure)?;
            Ok(socket)
        };
        match &self.context {
            Some(context) => {
                let mut peer = prepare(
                    &mut self.signaling,
                    async { context.accept(socket.await?).await },
                    3300,
                )
                .await?;
                replacement(&mut self.signaling, &mut peer, &self.uri, old, initial).await
            }
            None => {
                let mut peer = prepare(&mut self.signaling, socket, 3300).await?;
                replacement(&mut self.signaling, &mut peer, &self.uri, old, initial).await
            }
        }
    }

    async fn finish(mut self, primary: Result<(), ControlError>) -> Result<(), ControlError> {
        let mut result = cleanup::combine(primary, self.signaling.finish_registrations().await);
        result = cleanup::combine(result, self.signaling.finish_native().await);
        result = cleanup::combine(result, self.signaling.network.close().await);
        drop(self.signaling);
        let rebound = TcpListener::bind(self.local)
            .await
            .map(|_listener| ())
            .map_err(failure);
        cleanup::combine(result, rebound)
    }
}

async fn prepare<S>(
    signaling: &mut Signaling,
    accept: impl Future<Output = Result<S, ControlError>>,
    now: u64,
) -> Result<S, ControlError> {
    signaling.registration_tick(now).await?;
    let completed =
        tokio::time::timeout(Duration::from_secs(3), signaling.registrations.completed())
            .await
            .map_err(failure)??
            .ok_or(ControlError::Capacity)?;
    signaling.registration_completed(completed, now)?;
    let (event, peer) = tokio::time::timeout(Duration::from_secs(3), async {
        tokio::join!(signaling.network.next(), accept)
    })
    .await
    .map_err(failure)?;
    let Input::Prepared(prepared) = event? else {
        return Err(ControlError::Invalid(
            "Expected delayed registration preparation",
        ));
    };
    let peer = peer?;
    signaling.registration_prepared(prepared, now)?;
    Ok(peer)
}

async fn response<S: AsyncWrite + Unpin>(
    signaling: &mut Signaling,
    peer: &mut S,
    request: &Request,
    status: u16,
    extra: &str,
    now: u64,
) -> Result<(), ControlError> {
    peer.write_all(stream::reply(request, status, extra)?.as_bytes())
        .await
        .map_err(failure)?;
    let Input::Packet(packet) =
        tokio::time::timeout(Duration::from_secs(3), signaling.network.next())
            .await
            .map_err(failure)??
    else {
        return Err(ControlError::Invalid(
            "Expected delayed registration response",
        ));
    };
    if !signaling.registration_received(&packet, now) {
        return Err(ControlError::Invalid(
            "Delayed registration response was not consumed",
        ));
    }
    Ok(())
}

async fn receive<S: AsyncRead + Unpin>(
    peer: &mut S,
    decoder: &mut SipStreamDecoder,
    uri: &str,
) -> Result<stream::Packet, ControlError> {
    let packet = stream::receive(peer, decoder).await?;
    if packet.uri != uri {
        return Err(ControlError::Invalid(
            "Stream retry changed Request-URI wire octets",
        ));
    }
    Ok(packet)
}

async fn silent<S: AsyncRead + Unpin>(
    signaling: &mut Signaling,
    peer: &mut S,
    registrar: &TcpListener,
    now: u64,
) -> Result<(), ControlError> {
    signaling.registration_tick(now).await?;
    let mut byte = [0; 1];
    tokio::select! {
        biased;
        result = peer.read(&mut byte) => match result {
            Ok(0) => Err(ControlError::Invalid("Registration retry closed a shared flow")),
            Ok(_) => Err(ControlError::Invalid("Registration retried before its deadline or after cancellation")),
            Err(error) => Err(failure(error)),
        },
        result = registrar.accept() => match result {
            Ok(_) => Err(ControlError::Invalid("Registration retry opened an additional flow")),
            Err(error) => Err(failure(error)),
        },
        () = tokio::time::sleep(Duration::from_millis(30)) => Ok(()),
    }
}

async fn exchange<S: AsyncRead + AsyncWrite + Unpin>(
    signaling: &mut Signaling,
    peer: &mut S,
    registrar: &TcpListener,
    uri: &str,
    scenario: Scenario,
) -> Result<Option<(ConnectionKey, Request)>, ControlError> {
    signaling.registration_tick(1000).await?;
    let mut decoder = SipStreamDecoder::new();
    let initial = receive(peer, &mut decoder, uri).await?;
    if initial.request.cseq_header().map_err(failure)?.value() != "1 REGISTER" {
        return Err(ControlError::Invalid(
            "Stream retry changed its initial wire URI",
        ));
    }
    response(signaling, peer, &initial.request, 401, "WWW-Authenticate: Digest realm=\"rusterisk\", nonce=\"minimum-nonce\", algorithm=SHA-256, qop=\"auth-int\"\r\n", 1100).await?;
    signaling.registration_tick(1200).await?;
    let authenticated = receive(peer, &mut decoder, uri).await?;
    check(&authenticated.request, &initial.request, uri, 11, 2, 1)?;
    response(
        signaling,
        peer,
        &authenticated.request,
        500,
        "Retry-After: 2 (busy);duration=5\r\n",
        1300,
    )
    .await?;
    silent(signaling, peer, registrar, 3299).await?;
    match scenario {
        Scenario::Retry => {
            signaling.registration_tick(3300).await?;
            let retried = receive(peer, &mut decoder, uri).await?;
            check(&retried.request, &initial.request, uri, 11, 3, 2)?;
            let contact = retried.request.contact_header().map_err(failure)?.value();
            response(
                signaling,
                peer,
                &retried.request,
                200,
                &format!("Contact: {contact};expires=11\r\n"),
                3400,
            )
            .await?;
            silent(signaling, peer, registrar, 4399).await?;
            signaling.registration_tick(4400).await?;
            let refresh = receive(peer, &mut decoder, uri).await?;
            check(&refresh.request, &initial.request, uri, 11, 4, 3)?;
            Ok(None)
        }
        Scenario::Cancel => {
            signaling.registrations.cancel();
            silent(signaling, peer, registrar, 3300).await?;
            silent(signaling, peer, registrar, u64::MAX).await?;
            Ok(None)
        }
        Scenario::Disconnect => {
            peer.shutdown().await.map_err(failure)?;
            let Input::Closed { key, result } =
                tokio::time::timeout(Duration::from_secs(3), signaling.network.next())
                    .await
                    .map_err(failure)??
            else {
                return Err(ControlError::Invalid(
                    "Expected closed delayed registration flow",
                ));
            };
            result?;
            signaling.registration_closed(key);
            signaling.registration_tick(3299).await?;
            if !matches!(
                signaling.registrations.clients[0].state,
                State::Waiting { at: 3300 }
            ) {
                return Err(ControlError::Invalid(
                    "Closed flow changed the owned retry deadline",
                ));
            }
            match tokio::time::timeout(Duration::from_millis(30), registrar.accept()).await {
                Err(_) => Ok(Some((key, initial.request))),
                Ok(Err(error)) => Err(failure(error)),
                Ok(Ok(_)) => Err(ControlError::Invalid(
                    "Closed registration reconnected before its retry deadline",
                )),
            }
        }
    }
}

async fn replacement<S: AsyncRead + AsyncWrite + Unpin>(
    signaling: &mut Signaling,
    peer: &mut S,
    uri: &str,
    old: ConnectionKey,
    initial: &Request,
) -> Result<(), ControlError> {
    signaling.registration_closed(old);
    signaling.registration_tick(3300).await?;
    let mut decoder = SipStreamDecoder::new();
    let replacement = receive(peer, &mut decoder, uri).await?;
    if replacement.request.cseq_header().map_err(failure)?.value() != "3 REGISTER"
        || replacement
            .request
            .call_id_header()
            .map_err(failure)?
            .value()
            != initial.call_id_header().map_err(failure)?.value()
        || replacement
            .request
            .contact_header()
            .map_err(failure)?
            .value()
            != initial.contact_header().map_err(failure)?.value()
    {
        return Err(ControlError::Invalid(
            "Old connection changed replacement registration identity",
        ));
    }
    let contact = replacement
        .request
        .contact_header()
        .map_err(failure)?
        .value();
    response(
        signaling,
        peer,
        &replacement.request,
        200,
        &format!("Contact: {contact};expires=11\r\n"),
        3400,
    )
    .await?;
    if !matches!(
        signaling.registrations.clients[0].state,
        State::Registered { .. }
    ) {
        return Err(ControlError::Invalid(
            "Replacement flow did not accept its grant",
        ));
    }
    Ok(())
}

async fn run(protocol: Protocol, scenario: Scenario) -> Result<(), ControlError> {
    let mut fixture = Fixture::new(protocol).await?;
    let primary = async {
        if let Some((old, initial)) = fixture.exchange(scenario).await? {
            fixture.reconnect(old, &initial).await?;
        }
        Ok(())
    }
    .await;
    fixture.finish(primary).await
}

#[tokio::test]
async fn tcp_retry_after_authentication_keeps_one_flow_through_refresh() -> Result<(), ControlError>
{
    run(Protocol::Tcp, Scenario::Retry).await
}

#[tokio::test]
async fn tls_retry_after_authentication_keeps_one_verified_flow_through_refresh(
) -> Result<(), ControlError> {
    run(Protocol::Tls, Scenario::Retry).await
}

#[tokio::test]
async fn tcp_cancel_during_backoff_keeps_the_shared_flow_and_sends_no_bytes(
) -> Result<(), ControlError> {
    run(Protocol::Tcp, Scenario::Cancel).await
}

#[tokio::test]
async fn tls_cancel_during_backoff_keeps_the_verified_flow_and_sends_no_bytes(
) -> Result<(), ControlError> {
    run(Protocol::Tls, Scenario::Cancel).await
}

#[tokio::test]
async fn tcp_closed_backoff_waits_for_its_deadline_and_ignores_the_old_generation(
) -> Result<(), ControlError> {
    run(Protocol::Tcp, Scenario::Disconnect).await
}

#[tokio::test]
async fn tls_closed_backoff_waits_for_its_deadline_and_ignores_the_old_generation(
) -> Result<(), ControlError> {
    run(Protocol::Tls, Scenario::Disconnect).await
}
