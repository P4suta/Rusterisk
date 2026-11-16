use crate::{
    media::MediaHub,
    signaling::{
        cleanup,
        resolver::ResolvedTarget,
        transport::{ConnectionSetup, Input, SetupLifetime},
        Signaling,
    },
    ControlError, Controller,
};
use rusterisk_config::{Config, Protocol};
use rusterisk_sip::{Datagram, StaticTarget};
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

const OPTIONS: &[u8] = b"OPTIONS sip:localhost SIP/2.0\r\nContent-Length: 0\r\n\r\n";

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}

struct Fixture {
    signaling: Signaling,
    peers: Vec<TcpStream>,
}

impl Fixture {
    async fn new() -> Result<Self, ControlError> {
        let config = Config::parse(
            "[general]\nbindaddr=127.0.0.1\n",
            "[app]\ntype=user\npassword=test\n",
            "[native]\ntype=transport\nprotocol=tcp\nbind=127.0.0.1:0\n",
            "",
            "",
        )
        .map_err(failure)?;
        let (controller, _runner, effects) = Controller::new(config.clone(), 16);
        let (media, _hub, notices) = MediaHub::new(16);
        Ok(Self {
            signaling: Signaling::bind(config, controller, effects, media, notices).await?,
            peers: Vec::with_capacity(64),
        })
    }

    async fn populate(&mut self, count: usize) -> Result<(), ControlError> {
        if self
            .peers
            .len()
            .checked_add(count)
            .is_none_or(|total| total > 64)
        {
            return Err(ControlError::Capacity);
        }
        let address = self.signaling.network.local_addr(0)?;
        for _ in 0..count {
            let mut peer =
                tokio::time::timeout(Duration::from_secs(3), TcpStream::connect(address))
                    .await
                    .map_err(failure)?
                    .map_err(failure)?;
            peer.write_all(OPTIONS).await.map_err(failure)?;
            let Input::Packet(packet) =
                tokio::time::timeout(Duration::from_secs(3), self.signaling.network.next())
                    .await
                    .map_err(failure)??
            else {
                return Err(ControlError::Invalid("Expected an admitted stream frame"));
            };
            if packet.bytes != OPTIONS {
                return Err(ControlError::Invalid(
                    "Connection capacity changed the incoming frame",
                ));
            }
            self.peers.push(peer);
        }
        Ok(())
    }

    async fn finish(mut self, primary: Result<(), ControlError>) -> Result<(), ControlError> {
        let result = cleanup::combine(primary, self.signaling.finish_registrations().await);
        cleanup::combine(result, self.signaling.network.close().await)
    }
}

#[tokio::test]
async fn lookup_slots_remain_bounded_when_all_sixty_four_streams_are_open(
) -> Result<(), ControlError> {
    let mut fixture = Fixture::new().await?;
    let primary: Result<(), ControlError> = async {
        fixture.populate(64).await?;
        let mut keys = Vec::with_capacity(16);
        for _ in 0..16 {
            keys.push(
                fixture
                    .signaling
                    .network
                    .reserve_setup(0, fixture.signaling.registrations.generation())?,
            );
        }
        if !matches!(
            fixture.signaling.network.reserve_registered_setup(0),
            Err(ControlError::Capacity)
        ) {
            return Err(ControlError::Invalid(
                "Full streams bypassed the shared setup limit",
            ));
        }
        for key in &keys {
            fixture.signaling.network.cancel_setup(*key);
        }
        if !matches!(
            fixture.signaling.network.reserve_registered_setup(0),
            Err(ControlError::Capacity)
        ) {
            return Err(ControlError::Invalid(
                "Cancelled lookups released setup slots before completion",
            ));
        }
        let previous = keys.pop().ok_or(ControlError::Capacity)?;
        fixture.signaling.network.finish_resolution(previous);
        let replacement = fixture.signaling.network.reserve_registered_setup(0)?;
        if previous == replacement {
            return Err(ControlError::Invalid(
                "Lookup completion reused its setup generation",
            ));
        }
        Ok(())
    }
    .await;
    fixture.finish(primary).await
}

#[tokio::test]
async fn initial_setups_reuse_current_streams_at_full_connection_capacity(
) -> Result<(), ControlError> {
    let mut fixture = Fixture::new().await?;
    let primary: Result<(), ControlError> = async {
        fixture.populate(64).await?;
        let peer = fixture
            .peers
            .first()
            .ok_or(ControlError::Capacity)?
            .local_addr()
            .map_err(failure)?;
        let target =
            StaticTarget::from_uri(Protocol::Tcp, &format!("sip:{peer}")).map_err(failure)?;
        let resolved = ResolvedTarget::literal(&target)?.ok_or(ControlError::Capacity)?;
        let mut original = None;
        for _ in 0..16 {
            let setup = fixture
                .signaling
                .network
                .reserve_setup(0, fixture.signaling.registrations.generation())?;
            let lifetime =
                SetupLifetime::Configuration(fixture.signaling.registrations.lifetime.lease());
            let ConnectionSetup::Reused(key) =
                fixture
                    .signaling
                    .network
                    .connect_prepared(setup, resolved.clone(), lifetime)?
            else {
                return Err(ControlError::Invalid(
                    "Full capacity started another connection instead of reusing its flow",
                ));
            };
            if original.is_some_and(|previous| previous != key) {
                return Err(ControlError::Invalid(
                    "Reuse changed the admitted connection generation",
                ));
            }
            original = Some(key);
        }
        let route = fixture.signaling.network.capture(0, peer)?;
        if !fixture
            .signaling
            .network
            .send_on(route, Datagram::new(peer, OPTIONS.to_vec()))
            .await?
        {
            return Err(ControlError::Invalid(
                "Reused connection lost its delivery route",
            ));
        }
        let mut bytes = [0; OPTIONS.len()];
        tokio::time::timeout(
            Duration::from_secs(1),
            fixture
                .peers
                .first_mut()
                .ok_or(ControlError::Capacity)?
                .read_exact(&mut bytes),
        )
        .await
        .map_err(failure)?
        .map_err(failure)?;
        if bytes.as_slice() != OPTIONS {
            return Err(ControlError::Invalid(
                "Reused connection changed its packet",
            ));
        }
        for _ in 0..16 {
            fixture.signaling.network.reserve_registered_setup(0)?;
        }
        Ok(())
    }
    .await;
    fixture.finish(primary).await
}

#[tokio::test]
async fn cancelled_setups_cannot_reuse_or_close_shared_streams() -> Result<(), ControlError> {
    for configuration_ended in [false, true] {
        let mut fixture = Fixture::new().await?;
        let primary: Result<(), ControlError> = async {
            fixture.populate(1).await?;
            let peer = fixture
                .peers
                .first()
                .ok_or(ControlError::Capacity)?
                .local_addr()
                .map_err(failure)?;
            let target =
                StaticTarget::from_uri(Protocol::Tcp, &format!("sip:{peer}")).map_err(failure)?;
            let resolved = ResolvedTarget::literal(&target)?.ok_or(ControlError::Capacity)?;
            let setup = fixture
                .signaling
                .network
                .reserve_setup(0, fixture.signaling.registrations.generation())?;
            let lifetime =
                SetupLifetime::Configuration(fixture.signaling.registrations.lifetime.lease());
            if configuration_ended {
                fixture.signaling.registrations.lifetime.cancel();
            } else {
                fixture.signaling.network.cancel_setup(setup);
            }
            if !matches!(
                fixture
                    .signaling
                    .network
                    .connect_prepared(setup, resolved, lifetime),
                Err(ControlError::Conflict(_))
            ) {
                return Err(ControlError::Invalid(
                    "Cancelled setup published a reusable connection",
                ));
            }
            fixture.signaling.network.finish_resolution(setup);
            let route = fixture.signaling.network.capture(0, peer)?;
            if !fixture
                .signaling
                .network
                .send_on(route, Datagram::new(peer, OPTIONS.to_vec()))
                .await?
            {
                return Err(ControlError::Invalid(
                    "Cancelled setup closed its shared connection",
                ));
            }
            let mut bytes = [0; OPTIONS.len()];
            tokio::time::timeout(
                Duration::from_secs(1),
                fixture
                    .peers
                    .first_mut()
                    .ok_or(ControlError::Capacity)?
                    .read_exact(&mut bytes),
            )
            .await
            .map_err(failure)?
            .map_err(failure)?;
            if bytes.as_slice() != OPTIONS {
                return Err(ControlError::Invalid(
                    "Shared connection changed after setup cancellation",
                ));
            }
            Ok(())
        }
        .await;
        fixture.finish(primary).await?;
    }
    Ok(())
}
