use super::{failure, identity, join, require, VerifiedFlow};
use crate::{
    encryption::{TlsClientContext, TlsContext},
    media::MediaHub,
    signaling::{
        cleanup,
        resolver::ResolvedTarget,
        transport::{ConnectionAdmission, ConnectionSetup, Input, ListenerSocket, SetupLifetime},
        Signaling,
    },
    ControlError, Controller, Credentials, IncomingCall, IncomingTarget, Subscription,
};
use rsipstack::sip::{headers::typed::WwwAuthenticate, HeadersExt, SipMessage};
use rusterisk_api::ChannelId;
use rusterisk_config::{Config, Protocol};
use rusterisk_sip::StaticTarget;
use std::{future::Future, net::SocketAddr, num::NonZeroU64, time::Duration};
use tokio::{io::AsyncWriteExt, net::TcpListener, task::JoinHandle};

mod registered;

struct Fixture {
    signaling: Option<Signaling>,
    driver: Option<JoinHandle<Result<(), ControlError>>>,
    runner: JoinHandle<Result<(), ControlError>>,
    media: JoinHandle<Result<(), ControlError>>,
    controller: Controller,
    subscription: Option<Subscription>,
    peer: TcpListener,
    old_server: TlsContext,
    new_server: TlsContext,
    new_client: TlsClientContext,
    local: SocketAddr,
    flows: [Option<VerifiedFlow>; 2],
    directory: tempfile::TempDir,
}

fn preparation_failure(directory: tempfile::TempDir, primary: ControlError) -> ControlError {
    match directory.close().map_err(failure) {
        Ok(()) => primary,
        Err(cleanup) => ControlError::Adapter(format!("{primary}; TLS fixture cleanup: {cleanup}")),
    }
}

impl Fixture {
    async fn new() -> Result<Self, ControlError> {
        let credentials = Credentials::new("app".into(), "test".into())?;
        let directory = tempfile::tempdir().map_err(failure)?;
        let prepared = async {
            let (old_server, old_client) = identity(directory.path(), "driver-old").await?;
            let (new_server, new_client) = identity(directory.path(), "driver-new").await?;
            drop(old_client);
            let certificate = directory.path().join("driver-old.pem");
            let key = directory.path().join("driver-old-key.pem");
            let config = Config::parse(
                "[general]\nbindaddr=127.0.0.1\n",
                "[app]\ntype=user\npassword=test\n",
                &format!("[stream]\ntype=transport\nprotocol=tls\nbind=127.0.0.1:0\ncert_file={}\npriv_key_file={}\nca_list_file={}\n[auth]\ntype=auth\nusername=phone\npassword=signaling-secret\nrealm=rusterisk\n[phone]\ntype=aor\nmax_contacts=2\n[phone]\ntype=endpoint\ntransport=stream\nauth=auth\naors=phone\ncontext=internal\nallow=ulaw\n", certificate.display(), key.display(), certificate.display()),
                "", "[internal]\nexten => 100,1,Stasis(app)\n",
            ).map_err(failure)?;
            let peer = TcpListener::bind("127.0.0.1:0").await.map_err(failure)?;
            Ok::<_, ControlError>((config, peer, old_server, new_server, new_client))
        }.await;
        let (config, peer, old_server, new_server, new_client) = match prepared {
            Ok(prepared) => prepared,
            Err(primary) => return Err(preparation_failure(directory, primary)),
        };
        let (controller, runner, effects) = Controller::new(config.clone(), 8);
        let (media, hub, notices) = MediaHub::new(8);
        let signaling =
            match Signaling::bind(config, controller.clone(), effects, media, notices).await {
                Ok(signaling) => signaling,
                Err(primary) => return Err(preparation_failure(directory, primary)),
            };
        let local = match signaling.network.local_addr(0) {
            Ok(local) => local,
            Err(primary) => {
                drop(signaling);
                return Err(preparation_failure(directory, primary));
            }
        };
        let media_stop = controller.shutdown_signal();
        let runner = tokio::spawn(runner.run());
        let media = tokio::spawn(async move { hub.run(media_stop).await.map_err(failure) });
        let mut fixture = Self {
            signaling: Some(signaling),
            driver: None,
            runner,
            media,
            controller,
            subscription: None,
            peer,
            old_server,
            new_server,
            new_client,
            local,
            flows: [None, None],
            directory,
        };
        let subscribed = tokio::time::timeout(
            Duration::from_secs(2),
            fixture
                .controller
                .subscribe(credentials, None, "app".into()),
        )
        .await
        .map_err(failure)
        .and_then(|result| result);
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
        let call = tokio::time::timeout(
            Duration::from_secs(2),
            self.controller.incoming(IncomingCall {
                id: ChannelId::try_from(name.to_owned())?,
                endpoint: "phone".into(),
                caller: "displayed-caller".into(),
                context: "internal".into(),
                extension: "100".into(),
                target: IncomingTarget::Stasis {
                    application: "app".into(),
                    arguments: Vec::new(),
                },
            }),
        )
        .await
        .map_err(failure)??;
        let lease =
            tokio::time::timeout(Duration::from_secs(2), self.controller.call_lifetime(call))
                .await
                .map_err(failure)??;
        let signaling = self
            .signaling
            .as_mut()
            .ok_or(ControlError::Invalid("TLS fixture driver already started"))?;
        let setup = signaling.network.reserve_setup(0, generation)?;
        let peer = self.peer.local_addr().map_err(failure)?;
        let target =
            StaticTarget::from_uri(Protocol::Tls, &format!("sips:line@{peer};transport=tcp"))
                .map_err(failure)?;
        let target = ResolvedTarget::literal(&target)?.ok_or(ControlError::Invalid(
            "Expected a literal TLS fixture destination",
        ))?;
        require(
            matches!(
                signaling
                    .network
                    .connect_prepared(setup, target, SetupLifetime::Call(lease))?,
                ConnectionSetup::Connecting
            ),
            "TLS fixture reused a flow across starting trust profiles",
        )?;
        let network = &mut signaling.network;
        let accepted = async {
            let (socket, _) = self.peer.accept().await.map_err(failure)?;
            server.accept(socket).await
        };
        let admitted = async {
            for _ in 0..16 {
                match network.next().await? {
                    Input::Prepared(prepared) => {
                        require(
                            prepared.key == setup,
                            "TLS fixture received another setup completion",
                        )?;
                        match network.admit_prepared(prepared)? {
                            ConnectionAdmission::Ready(key) => return Ok(key),
                            ConnectionAdmission::Continue => {}
                        }
                    }
                    Input::SetupFinished { key, result } => {
                        require(
                            key == setup,
                            "TLS fixture setup completion changed generation",
                        )?;
                        result?;
                        return Err(ControlError::Invalid(
                            "TLS fixture setup ended before admission",
                        ));
                    }
                    Input::Closed { result, .. } => {
                        result?;
                        return Err(ControlError::Invalid(
                            "TLS fixture flow closed before admission",
                        ));
                    }
                    Input::Packet(_) | Input::Upgrade(_) => {
                        return Err(ControlError::Invalid(
                            "SIP arrived before the TLS fixture driver started",
                        ))
                    }
                }
            }
            Err(ControlError::Capacity)
        };
        let (socket, key) = tokio::time::timeout(Duration::from_secs(3), async {
            tokio::try_join!(accepted, admitted)
        })
        .await
        .map_err(failure)??;
        require(
            key.listener == 0 && key.peer == peer,
            "TLS fixture flow changed its physical peer",
        )?;
        Ok(VerifiedFlow {
            key,
            socket,
            decoder: rusterisk_sip::SipStreamDecoder::new(),
        })
    }

    async fn prepare_flows(&mut self) -> Result<(), ControlError> {
        self.flows[0] = Some(
            self.connect(NonZeroU64::MIN, "driver-old", self.old_server.clone())
                .await?,
        );
        let signaling = self
            .signaling
            .as_mut()
            .ok_or(ControlError::Invalid("TLS fixture driver already started"))?;
        let Some(ListenerSocket::Encrypted { client, .. }) =
            signaling.network.listeners.first_mut()
        else {
            return Err(ControlError::Invalid(
                "Expected the owned TLS fixture listener",
            ));
        };
        *client = self.new_client.clone();
        self.flows[1] = Some(
            self.connect(
                NonZeroU64::new(2).ok_or(ControlError::Capacity)?,
                "driver-new",
                self.new_server.clone(),
            )
            .await?,
        );
        let old = self.flows[0].as_ref().ok_or(ControlError::Capacity)?;
        let new = self.flows[1].as_ref().ok_or(ControlError::Capacity)?;
        require(
            old.key != new.key && old.key.peer == new.key.peer,
            "TLS fixture did not own two same-peer connection generations",
        )
    }

    fn start(&mut self) -> Result<(), ControlError> {
        let signaling = self
            .signaling
            .take()
            .ok_or(ControlError::Invalid("TLS fixture driver already started"))?;
        self.driver = Some(tokio::spawn(
            signaling.run(self.controller.shutdown_signal()),
        ));
        Ok(())
    }

    async fn exchange(&mut self, index: usize, wire: &[u8]) -> Result<Vec<u8>, ControlError> {
        let flow = self
            .flows
            .get_mut(index)
            .and_then(Option::as_mut)
            .ok_or(ControlError::Invalid("Missing owned TLS fixture flow"))?;
        tokio::time::timeout(Duration::from_secs(2), flow.socket.write_all(wire))
            .await
            .map_err(failure)?
            .map_err(failure)?;
        flow.message().await.map_err(|error| {
            ControlError::Adapter(format!(
                "Signaling dispatch response on TLS flow {index}: {error}"
            ))
        })
    }

    fn finish(
        mut self,
        primary: Result<(), ControlError>,
    ) -> impl Future<Output = Result<(), ControlError>> {
        self.controller.shutdown();
        let mut result = primary;
        if self.signaling.is_some() {
            result = cleanup::combine(result, self.start());
        }
        let Self {
            signaling,
            driver,
            runner,
            media,
            controller,
            subscription,
            peer,
            old_server,
            new_server,
            new_client,
            local,
            flows,
            directory,
        } = self;
        drop(signaling);
        async move {
            let mut result = result;
            for task in [driver, Some(runner), Some(media)].into_iter().flatten() {
                result = cleanup::combine(result, join(task).await);
            }
            let remote = peer.local_addr().map_err(failure);
            drop((
                flows,
                subscription,
                controller,
                peer,
                old_server,
                new_server,
                new_client,
            ));
            for address in [Ok(local), remote] {
                let rebound = match address {
                    Ok(address) => TcpListener::bind(address).await.map(drop).map_err(failure),
                    Err(error) => Err(error),
                };
                result = cleanup::combine(result, rebound);
            }
            cleanup::combine(result, directory.close().map_err(failure))
        }
    }
}

fn invitation(local: SocketAddr) -> String {
    let body = "v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=call\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\nm=audio 19000 RTP/AVP 0\r\na=rtpmap:0 PCMU/8000\r\na=sendrecv\r\n";
    format!("INVITE sip:100@localhost SIP/2.0\r\nVia: SIP/2.0/TLS {local};branch=z9hG4bK-driver-invite;rport\r\nFrom: Display <sip:shown-caller@localhost>;tag=driver-client\r\nTo: <sip:100@localhost>\r\nCall-ID: driver-shared-invite\r\nCSeq: 1 INVITE\r\nContact: <sip:phone@{local};transport=tcp>\r\nMax-Forwards: 70\r\nContent-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{body}", body.len())
}

fn nonce(bytes: &[u8]) -> Result<String, ControlError> {
    let SipMessage::Response(response) = SipMessage::try_from(bytes).map_err(failure)? else {
        return Err(ControlError::Invalid(
            "TLS INVITE did not receive a SIP response",
        ));
    };
    require(
        response.status_code.code() == 401,
        "TLS INVITE did not receive its independent challenge",
    )?;
    require(
        response.call_id_header().map_err(failure)?.value() == "driver-shared-invite"
            && response.cseq_header().map_err(failure)?.value() == "1 INVITE",
        "TLS challenge changed the original INVITE identity",
    )?;
    let challenge = WwwAuthenticate::parse(
        response
            .www_authenticate_header()
            .ok_or(ControlError::Invalid(
                "TLS challenge omitted digest authentication",
            ))?
            .value(),
    )
    .map_err(failure)?;
    Ok(challenge.nonce)
}

#[tokio::test]
async fn actual_signaling_dispatch_keeps_same_peer_tls_challenges_and_cache_on_their_flows(
) -> Result<(), ControlError> {
    let mut fixture = Fixture::new().await?;
    let primary = async {
        fixture.prepare_flows().await?;
        let wire = invitation(fixture.peer.local_addr().map_err(failure)?);
        fixture.start()?;
        let old = fixture.exchange(0, wire.as_bytes()).await?;
        let new = fixture.exchange(1, wire.as_bytes()).await?;
        require(
            nonce(&old)? != nonce(&new)?,
            "Actual Signaling dispatch shared a nonce across verified TLS flows",
        )?;
        require(
            fixture.exchange(0, wire.as_bytes()).await? == old,
            "Old TLS flow did not receive its original cached challenge",
        )?;
        require(
            fixture.exchange(1, wire.as_bytes()).await? == new,
            "New TLS flow did not receive its original cached challenge",
        )?;
        Ok(())
    }
    .await;
    fixture.finish(primary).await
}
