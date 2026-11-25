use super::{failure, Fixture as TransportFixture};
use crate::{
    encryption::{TlsClientContext, TlsContext},
    signaling::{
        resolver::ResolvedTarget,
        transport::{ConnectionAdmission, ConnectionKey, ConnectionSetup, Input, SetupLifetime},
    },
    ControlError, Controller, Credentials, IncomingCall, IncomingTarget, Subscription,
};
use rusterisk_api::ChannelId;
use rusterisk_config::{Config, Protocol, TlsFiles};
use rusterisk_sip::{Datagram, SipStreamDecoder, StaticTarget, StreamItem};
use std::{net::SocketAddr, num::NonZeroU64, path::Path};
use tokio::{
    io::AsyncReadExt,
    net::{TcpListener, TcpStream},
    task::JoinHandle,
};
use tokio_rustls::server::TlsStream;

mod closed;
mod driver;
mod received;

fn require(condition: bool, message: &'static str) -> Result<(), ControlError> {
    if condition {
        Ok(())
    } else {
        Err(ControlError::Invalid(message))
    }
}

async fn identity(
    directory: &Path,
    name: &str,
) -> Result<(TlsContext, TlsClientContext), ControlError> {
    let identity = rcgen::generate_simple_self_signed(["127.0.0.1".into()]).map_err(failure)?;
    let certificate = directory.join(format!("{name}.pem"));
    let private_key = directory.join(format!("{name}-key.pem"));
    std::fs::write(&certificate, identity.cert.pem()).map_err(failure)?;
    std::fs::write(&private_key, identity.signing_key.serialize_pem()).map_err(failure)?;
    let files = TlsFiles {
        certificate: certificate.to_string_lossy().into_owned(),
        private_key: private_key.to_string_lossy().into_owned(),
    };
    Ok((
        TlsContext::load(&files).await?,
        TlsClientContext::load_sip(Some(&files.certificate))?,
    ))
}

struct VerifiedFlow {
    key: ConnectionKey,
    socket: TlsStream<TcpStream>,
    decoder: SipStreamDecoder,
}

impl VerifiedFlow {
    async fn message(&mut self) -> Result<Vec<u8>, ControlError> {
        rusterisk_test_support::Wait::Ready
            .bounded(async {
                let mut byte = [0; 1];
                loop {
                    require(
                        self.socket.read(&mut byte).await.map_err(failure)? == 1,
                        "Verified TLS flow closed before its captured delivery",
                    )?;
                    match self.decoder.push(&byte).map_err(failure)?.1 {
                        Some(StreamItem::Message(bytes)) => return Ok(bytes),
                        Some(StreamItem::Crlf) | None => {}
                    }
                }
            })
            .await
            .map_err(failure)?
    }
}

struct Fixture {
    transport: TransportFixture,
    controller: Controller,
    runner: JoinHandle<Result<(), ControlError>>,
    subscription: Option<Subscription>,
    effects: tokio::sync::mpsc::Receiver<crate::Effect>,
    peer: TcpListener,
    old_server: TlsContext,
    new_server: TlsContext,
}

impl Fixture {
    async fn new() -> Result<Self, ControlError> {
        let mut transport = TransportFixture::new().await?;
        let (old_server, old_client) = identity(transport._directory.path(), "flow-old").await?;
        let (new_server, new_client) = identity(transport._directory.path(), "flow-new").await?;
        let Some(super::super::super::ListenerSocket::Encrypted {
            context, client, ..
        }) = transport.network.listeners.first_mut()
        else {
            return Err(ControlError::Invalid("Expected an owned TLS listener"));
        };
        *context = old_server.clone();
        *client = old_client;
        transport.changed = new_client;
        let peer = TcpListener::bind("127.0.0.1:0").await.map_err(failure)?;
        let config = Config::parse(
            "[general]\nbindaddr=127.0.0.1\n",
            "[app]\ntype=user\npassword=test\n",
            "",
            "",
            "",
        )
        .map_err(failure)?;
        let (controller, runner, effects) = Controller::new(config, 4);
        let runner = tokio::spawn(runner.run());
        let mut fixture = Self {
            transport,
            controller,
            runner,
            subscription: None,
            effects,
            peer,
            old_server,
            new_server,
        };
        let subscribed = fixture
            .controller
            .subscribe(
                Credentials::new("app".into(), "test".into())?,
                None,
                "app".into(),
            )
            .await;
        match subscribed {
            Ok(subscription) => {
                fixture.subscription = Some(subscription);
                Ok(fixture)
            }
            Err(primary) => {
                let fallback = primary.clone();
                match fixture.finish(Err(primary)).await {
                    Err(error) => Err(error),
                    Ok(()) => Err(fallback),
                }
            }
        }
    }

    async fn connect(
        &mut self,
        generation: NonZeroU64,
        name: &str,
        server: TlsContext,
    ) -> Result<VerifiedFlow, ControlError> {
        let call = self
            .controller
            .incoming(IncomingCall {
                id: ChannelId::try_from(name.to_owned())?,
                endpoint: "phone".into(),
                caller: "100".into(),
                context: "internal".into(),
                extension: "100".into(),
                target: IncomingTarget::Stasis {
                    application: "app".into(),
                    arguments: Vec::new(),
                },
            })
            .await?;
        let lease = self.controller.call_lifetime(call).await?;
        let setup = self.transport.network.reserve_setup(0, generation)?;
        let peer = self.peer.local_addr().map_err(failure)?;
        let target =
            StaticTarget::from_uri(Protocol::Tls, &format!("sips:line@{peer};transport=tcp"))
                .map_err(failure)?;
        let resolved = ResolvedTarget::literal(&target)?
            .ok_or(ControlError::Invalid("Expected a literal TLS destination"))?;
        require(
            matches!(
                self.transport.network.connect_prepared(
                    setup,
                    resolved,
                    SetupLifetime::Call(lease)
                )?,
                ConnectionSetup::Connecting
            ),
            "Changed TLS profile reused an existing flow before verification",
        )?;
        let network = &mut self.transport.network;
        let listener = &self.peer;
        let accepted = async {
            let (socket, _) = listener.accept().await.map_err(failure)?;
            server.accept(socket).await
        };
        let admitted = async {
            for _ in 0..16 {
                match network.next().await? {
                    Input::Prepared(prepared) => {
                        require(
                            prepared.key == setup,
                            "TLS completion changed its setup generation",
                        )?;
                        match network.admit_prepared(prepared)? {
                            ConnectionAdmission::Ready(key) => return Ok(key),
                            ConnectionAdmission::Continue => {}
                        }
                    }
                    Input::SetupFinished { key, result } => {
                        require(key == setup, "Unexpected TLS setup completion")?;
                        result?;
                        return Err(ControlError::Invalid(
                            "TLS setup ended without its verified flow",
                        ));
                    }
                    Input::Closed { result, .. } => {
                        result?;
                        return Err(ControlError::Invalid(
                            "An owned TLS flow closed during admission",
                        ));
                    }
                    Input::Packet(_) | Input::Upgrade(_) => {
                        return Err(ControlError::Invalid(
                            "Unexpected SIP packet during TLS admission",
                        ))
                    }
                }
            }
            Err(ControlError::Capacity)
        };
        let (socket, key) = rusterisk_test_support::Wait::Ready
            .bounded(async { tokio::try_join!(accepted, admitted) })
            .await
            .map_err(failure)??;
        require(
            key.listener == 0 && key.peer == peer,
            "Verified TLS flow changed its physical destination",
        )?;
        Ok(VerifiedFlow {
            key,
            socket,
            decoder: SipStreamDecoder::new(),
        })
    }

    async fn finish(self, primary: Result<(), ControlError>) -> Result<(), ControlError> {
        let Self {
            transport,
            controller,
            runner,
            subscription,
            effects,
            peer,
            old_server,
            new_server,
        } = self;
        let TransportFixture {
            mut network,
            changed,
            _directory: directory,
        } = transport;
        let local = network.local_addr(0);
        let remote = peer.local_addr().map_err(failure);
        controller.shutdown();
        let mut result = crate::signaling::cleanup::combine(primary, network.close().await);
        result = crate::signaling::cleanup::combine(result, join(runner).await);
        drop((
            network,
            subscription,
            effects,
            controller,
            peer,
            old_server,
            new_server,
            changed,
        ));
        for address in [local, remote] {
            let rebound = match address {
                Ok(address) => TcpListener::bind(address).await.map(drop).map_err(failure),
                Err(error) => Err(error),
            };
            result = crate::signaling::cleanup::combine(result, rebound);
        }
        crate::signaling::cleanup::combine(result, directory.close().map_err(failure))
    }
}

async fn join(mut task: JoinHandle<Result<(), ControlError>>) -> Result<(), ControlError> {
    match rusterisk_test_support::Wait::Ready.bounded(&mut task).await {
        Ok(result) => result.map_err(failure)?,
        Err(timeout) => {
            task.abort();
            let cleanup = match task.await {
                Ok(result) => result,
                Err(error) if error.is_cancelled() => Ok(()),
                Err(error) => Err(failure(error)),
            };
            crate::signaling::cleanup::combine(Err(failure(timeout)), cleanup)
        }
    }
}

fn wire(local: SocketAddr, name: &str) -> String {
    format!("OPTIONS sip:{name}@127.0.0.1 SIP/2.0\r\nVia: SIP/2.0/TLS {local};branch=z9hG4bK-{name}\r\nFrom: <sip:peer@127.0.0.1>;tag={name}\r\nTo: <sip:service@127.0.0.1>\r\nCall-ID: {name}\r\nCSeq: 1 OPTIONS\r\nMax-Forwards: 70\r\nContent-Length: 0\r\n\r\n")
}

#[tokio::test]
async fn verified_tls_profiles_coexist_without_redirecting_a_captured_old_route(
) -> Result<(), ControlError> {
    let mut fixture = Fixture::new().await?;
    let primary = async {
        let mut old = fixture
            .connect(NonZeroU64::MIN, "old", fixture.old_server.clone())
            .await?;
        let route = fixture
            .transport
            .network
            .capture(old.key.listener, old.key.peer)?;
        fixture.transport.replace_profile()?;
        let new = fixture
            .connect(
                NonZeroU64::new(2).ok_or(ControlError::Capacity)?,
                "new",
                fixture.new_server.clone(),
            )
            .await?;
        require(
            old.key != new.key
                && old.key.peer == new.key.peer
                && old.key.listener == new.key.listener,
            "TLS profiles did not retain two distinct verified flows",
        )?;
        require(
            fixture.transport.network.current(route),
            "New TLS admission replaced the captured old flow",
        )?;
        let bytes = wire(fixture.transport.network.local_addr(0)?, "captured-old");
        require(
            fixture
                .transport
                .network
                .send_on(route, Datagram::new(old.key.peer, bytes.as_bytes().into()))
                .await?,
            "Captured old TLS flow was unavailable",
        )?;
        require(
            old.message().await? == bytes.as_bytes(),
            "Captured route sent bytes on the replacement TLS flow",
        )?;
        drop(new);
        Ok(())
    }
    .await;
    fixture.finish(primary).await
}
