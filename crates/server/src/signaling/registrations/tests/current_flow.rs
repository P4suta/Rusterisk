use super::{
    stream::{receive, reply, Packet},
    tls::{configuration, Identity},
};
use crate::{
    encryption::TlsContext,
    media::MediaHub,
    signaling::{
        cleanup,
        registrations::{resolution::ConfigurationLifetime, Route, State},
        resolver::ResolvedTarget,
        transport::{
            ConnectionAdmission, ConnectionSetup, DeliveryRoute, Input, Prepared, SetupLifetime,
        },
        Received, Signaling,
    },
    ControlError, Controller,
};
use rsipstack::sip::HeadersExt;
use rusterisk_config::Protocol;
use rusterisk_sip::{SipStreamDecoder, StaticTarget};
use std::{num::NonZeroU64, time::Duration};
use tokio::{
    io::AsyncWriteExt,
    net::{TcpListener, TcpStream},
};
use tokio_rustls::server::TlsStream;

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}

fn require(condition: bool, message: &'static str) -> Result<(), ControlError> {
    if condition {
        Ok(())
    } else {
        Err(ControlError::Invalid(message))
    }
}

struct Flow {
    socket: TlsStream<TcpStream>,
    route: DeliveryRoute,
}

struct Fixture {
    signaling: Signaling,
    registrar: TcpListener,
    context: TlsContext,
    identity: Identity,
    alternate_lifetime: ConfigurationLifetime,
}

impl Fixture {
    async fn new() -> Result<Self, ControlError> {
        let identity = Identity::new("127.0.0.1")?;
        let context = TlsContext::load(&identity.files).await?;
        let registrar = TcpListener::bind("127.0.0.1:0").await.map_err(failure)?;
        let (config, _uri) = configuration(
            registrar.local_addr().map_err(failure)?,
            &identity,
            &identity,
        )?;
        let (controller, runner, _effects) = Controller::new(config.clone(), 16);
        let (media, _hub, notices) = MediaHub::new(16);
        let signaling = Signaling::bind(config, controller, runner, media, notices).await?;
        Ok(Self {
            signaling,
            registrar,
            context,
            identity,
            alternate_lifetime: ConfigurationLifetime::new(
                NonZeroU64::new(2).ok_or(ControlError::Capacity)?,
            ),
        })
    }

    fn trying_route(&self) -> Result<DeliveryRoute, ControlError> {
        let client = self
            .signaling
            .registrations
            .clients
            .first()
            .ok_or(ControlError::Capacity)?;
        let State::Trying {
            route: Route::Stream(route),
            ..
        } = &client.state
        else {
            return Err(ControlError::Invalid(
                "Expected a live stream REGISTER transaction",
            ));
        };
        Ok(*route)
    }

    async fn accept_prepared(&mut self) -> Result<(TlsStream<TcpStream>, Prepared), ControlError> {
        let network = &mut self.signaling.network;
        let registrar = &self.registrar;
        let context = &self.context;
        let accepted = async {
            let (socket, _peer) = registrar.accept().await.map_err(failure)?;
            context.accept(socket).await
        };
        let prepared = async {
            match network.next().await? {
                Input::Prepared(prepared) => Ok(prepared),
                Input::Closed { result, .. } | Input::SetupFinished { result, .. } => {
                    result?;
                    Err(ControlError::Invalid("TLS flow ended before admission"))
                }
                Input::Packet(_) | Input::Upgrade(_) => Err(ControlError::Invalid(
                    "Unexpected packet during TLS admission",
                )),
            }
        };
        tokio::time::timeout(Duration::from_secs(3), async {
            tokio::try_join!(accepted, prepared)
        })
        .await
        .map_err(failure)?
    }

    async fn registration(&mut self) -> Result<(Flow, Packet), ControlError> {
        self.signaling.registration_tick(1000).await?;
        let completed = tokio::time::timeout(
            Duration::from_secs(3),
            self.signaling.registrations.completed(),
        )
        .await
        .map_err(failure)??
        .ok_or(ControlError::Capacity)?;
        self.signaling.registration_completed(completed, 1000)?;
        let (mut socket, prepared) = self.accept_prepared().await?;
        self.signaling.registration_prepared(prepared, 1000)?;
        self.signaling.registration_tick(1000).await?;
        let route = self.trying_route()?;
        let initial = receive(&mut socket, &mut SipStreamDecoder::new()).await?;
        Ok((Flow { socket, route }, initial))
    }

    async fn alternate(&mut self) -> Result<Flow, ControlError> {
        let peer = self.registrar.local_addr().map_err(failure)?;
        let target = StaticTarget::from_uri(Protocol::Tls, &format!("sips:account@{peer}"))
            .map_err(failure)?;
        let resolved = ResolvedTarget::literal(&target)?
            .ok_or(ControlError::Invalid("Expected a literal TLS registrar"))?;
        let setup = self
            .signaling
            .network
            .reserve_setup(0, self.alternate_lifetime.generation())?;
        require(
            matches!(
                self.signaling.network.connect_prepared(
                    setup,
                    resolved,
                    SetupLifetime::Configuration(self.alternate_lifetime.lease()),
                )?,
                ConnectionSetup::Connecting
            ),
            "A distinct TLS epoch reused the REGISTER flow",
        )?;
        let (socket, prepared) = self.accept_prepared().await?;
        require(
            prepared.key == setup,
            "TLS admission changed the setup identity",
        )?;
        let ConnectionAdmission::Ready(key) = self.signaling.network.admit_prepared(prepared)?
        else {
            return Err(ControlError::Invalid(
                "Expected an admitted alternate TLS flow",
            ));
        };
        Ok(Flow {
            socket,
            route: self.signaling.network.capture_connection(key)?,
        })
    }

    async fn packet(&mut self, flow: &mut Flow, bytes: &[u8]) -> Result<Received, ControlError> {
        tokio::time::timeout(Duration::from_secs(3), async {
            flow.socket.write_all(bytes).await.map_err(failure)?;
            for _ in 0..16 {
                match self.signaling.network.next().await? {
                    Input::Packet(packet) => return Ok(packet),
                    Input::Closed { result, .. } => {
                        result?;
                        return Err(ControlError::Invalid("TLS flow closed before its response"));
                    }
                    Input::SetupFinished { result, .. } => result?,
                    Input::Prepared(_) | Input::Upgrade(_) => {
                        return Err(ControlError::Invalid(
                            "Unexpected additional TLS preparation",
                        ));
                    }
                }
            }
            Err(ControlError::Capacity)
        })
        .await
        .map_err(failure)?
    }

    async fn finish(self, primary: Result<(), ControlError>) -> Result<(), ControlError> {
        let Self {
            mut signaling,
            registrar,
            context,
            identity,
            alternate_lifetime,
        } = self;
        let local = signaling.network.local_addr(0);
        let peer = registrar.local_addr().map_err(failure);
        let mut result = cleanup::combine(primary, signaling.finish_registrations().await);
        result = cleanup::combine(result, signaling.finish_native().await);
        result = cleanup::combine(result, signaling.network.close().await);
        drop((signaling, registrar, context, identity, alternate_lifetime));
        for address in [local, peer] {
            let rebound = match address {
                Ok(address) => TcpListener::bind(address).await.map(drop).map_err(failure),
                Err(error) => Err(error),
            };
            result = cleanup::combine(result, rebound);
        }
        result
    }
}

#[tokio::test]
async fn current_registration_rejects_an_exact_response_from_another_live_same_peer_flow(
) -> Result<(), ControlError> {
    let mut fixture = Fixture::new().await?;
    let primary = async {
        let (mut owner, initial) = fixture.registration().await?;
        let mut alternate = fixture.alternate().await?;
        require(
            owner.route != alternate.route
                && owner.route.listener() == alternate.route.listener()
                && owner.route.peer() == alternate.route.peer()
                && fixture.signaling.network.current(owner.route)
                && fixture.signaling.network.current(alternate.route),
            "The fixture requires two distinct live flows to one registrar",
        )?;
        let contact = initial.request.contact_header().map_err(failure)?.value();
        let response = reply(
            &initial.request,
            200,
            &format!("Contact: {contact};expires=60\r\nExpires: 60\r\n"),
        )?;
        let received = fixture.packet(&mut alternate, response.as_bytes()).await?;
        require(
            received.route() == alternate.route
                && received.peer == owner.route.peer()
                && received.bytes == response.as_bytes(),
            "Network ingress changed the alternate flow response",
        )?;
        require(
            !fixture.signaling.registration_received(&received, 1001)
                && fixture.trying_route()? == owner.route
                && fixture.signaling.network.current(owner.route)
                && fixture.signaling.network.current(alternate.route),
            "Another live flow settled the current REGISTER transaction",
        )?;
        let received = fixture.packet(&mut owner, response.as_bytes()).await?;
        require(
            received.route() == owner.route && received.bytes == response.as_bytes(),
            "Network ingress changed the original flow response",
        )?;
        require(
            fixture.signaling.registration_received(&received, 1002),
            "The starting flow lost its matching registration response",
        )?;
        require(
            fixture.signaling.registrations.clients.first().is_some_and(|client| {
                matches!(client.state, State::Registered { route: Route::Stream(route), .. } if route == owner.route)
            }),
            "The original flow did not complete its current registration",
        )
    }
    .await;
    fixture.finish(primary).await
}
