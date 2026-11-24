use super::stream::{configuration, receive, reply};
use crate::{
    media::MediaHub,
    signaling::{
        cleanup,
        registrations::{resolution::ConfigurationLifetime, State},
        transport::{Input, Prepared},
        Signaling,
    },
    ControlError, Controller,
};
use rsipstack::sip::HeadersExt;
use rusterisk_sip::SipStreamDecoder;
use std::{num::NonZeroU64, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}

struct Fixture {
    signaling: Signaling,
    registrar: TcpListener,
}

impl Fixture {
    async fn new() -> Result<Self, ControlError> {
        let registrar = TcpListener::bind("127.0.0.1:0").await.map_err(failure)?;
        let uri = format!(
            "sip:{};transport=tcp",
            registrar.local_addr().map_err(failure)?
        );
        let config = configuration(&uri)?;
        let (controller, runner, _effects) = Controller::new(config.clone(), 16);
        let (media, _hub, notices) = MediaHub::new(16);
        let signaling = Signaling::bind(config, controller, runner, media, notices).await?;
        Ok(Self {
            signaling,
            registrar,
        })
    }

    async fn prepared(&mut self, now: u64) -> Result<(Prepared, TcpStream), ControlError> {
        self.signaling.registration_tick(now).await?;
        let completed = tokio::time::timeout(
            Duration::from_secs(3),
            self.signaling.registrations.completed(),
        )
        .await
        .map_err(failure)??
        .ok_or(ControlError::Capacity)?;
        self.signaling.registration_completed(completed, now)?;
        let Input::Prepared(prepared) =
            tokio::time::timeout(Duration::from_secs(3), self.signaling.network.next())
                .await
                .map_err(failure)??
        else {
            return Err(ControlError::Invalid(
                "Expected registration connection preparation",
            ));
        };
        let (peer, _address) =
            tokio::time::timeout(Duration::from_secs(3), self.registrar.accept())
                .await
                .map_err(failure)?
                .map_err(failure)?;
        Ok((prepared, peer))
    }

    async fn finish(mut self, primary: Result<(), ControlError>) -> Result<(), ControlError> {
        let result = cleanup::combine(primary, self.signaling.finish_registrations().await);
        cleanup::combine(result, self.signaling.network.close().await)
    }
}

#[tokio::test]
async fn configuration_cancelled_before_the_connection_job_is_polled_never_connects(
) -> Result<(), ControlError> {
    let mut fixture = Fixture::new().await?;
    let primary: Result<(), ControlError> = async {
        for generation in 1..=16 {
            fixture.signaling.registrations.lifetime = ConfigurationLifetime::new(
                NonZeroU64::new(generation).ok_or(ControlError::Capacity)?,
            );
            let now = 1000 + generation;
            fixture.signaling.registrations.clients[0].state = State::Waiting { at: now };
            fixture.signaling.registration_tick(now).await?;
            let completed = tokio::time::timeout(
                Duration::from_secs(3),
                fixture.signaling.registrations.completed(),
            )
            .await
            .map_err(failure)??
            .ok_or(ControlError::Capacity)?;
            fixture.signaling.registration_completed(completed, now)?;
            fixture.signaling.registrations.lifetime.cancel();
            let Input::SetupFinished { key, result } =
                tokio::time::timeout(Duration::from_secs(3), fixture.signaling.network.next())
                    .await
                    .map_err(failure)??
            else {
                return Err(ControlError::Invalid(
                    "Cancelled configuration published a TCP preparation",
                ));
            };
            fixture.signaling.registration_setup_finished(key, result);
            if tokio::time::timeout(Duration::from_millis(50), fixture.registrar.accept())
                .await
                .is_ok()
            {
                return Err(ControlError::Invalid(
                    "Cancelled configuration initiated a TCP connection",
                ));
            }
        }
        Ok(())
    }
    .await;
    fixture.finish(primary).await
}

#[tokio::test]
async fn cancelled_configuration_cannot_admit_a_prepared_tcp_flow_or_send_registration_bytes(
) -> Result<(), ControlError> {
    let mut fixture = Fixture::new().await?;
    let primary: Result<(), ControlError> = async {
        let (prepared, mut peer) = fixture.prepared(1000).await?;
        fixture.signaling.registrations.lifetime.cancel();
        fixture.signaling.registration_prepared(prepared, 1001)?;
        let event = tokio::time::timeout(Duration::from_secs(3), fixture.signaling.network.next())
            .await
            .map_err(failure)??;
        if !matches!(event, Input::SetupFinished { .. })
            || !matches!(
                fixture.signaling.registrations.clients[0].state,
                State::Stopped
            )
        {
            return Err(ControlError::Invalid(
                "Cancelled TCP preparation changed registration state",
            ));
        }
        let count = tokio::time::timeout(Duration::from_secs(1), peer.read(&mut [0; 1]))
            .await
            .map_err(failure)?
            .map_err(failure)?;
        if count != 0 {
            return Err(ControlError::Invalid(
                "Cancelled preparation sent registration bytes",
            ));
        }
        let recovered = fixture
            .signaling
            .network
            .reserve_setup(0, NonZeroU64::MIN)?;
        fixture.signaling.network.finish_resolution(recovered);
        Ok(())
    }
    .await;
    fixture.finish(primary).await
}

#[tokio::test]
async fn disconnected_registrations_stop_refresh_and_ignore_old_connection_generations(
) -> Result<(), ControlError> {
    let mut fixture = Fixture::new().await?;
    let primary: Result<(), ControlError> = async {
        let (prepared, mut peer) = fixture.prepared(1000).await?;
        fixture.signaling.registration_prepared(prepared, 1000)?;
        fixture.signaling.registration_tick(1000).await?;
        let mut decoder = SipStreamDecoder::new();
        let initial = receive(&mut peer, &mut decoder).await?;
        let contact = initial.request.contact_header().map_err(failure)?.value();
        let grant = reply(
            &initial.request,
            200,
            &format!("Contact: {contact};expires=11\r\nExpires: 11\r\n"),
        )?;
        peer.write_all(grant.as_bytes()).await.map_err(failure)?;
        let Input::Packet(packet) =
            tokio::time::timeout(Duration::from_secs(3), fixture.signaling.network.next())
                .await
                .map_err(failure)??
        else {
            return Err(ControlError::Invalid("Expected registration grant"));
        };
        if !fixture.signaling.registration_received(&packet, 1001)
            || !matches!(
                fixture.signaling.registrations.clients[0].state,
                State::Registered { .. }
            )
        {
            return Err(ControlError::Invalid(
                "Registration did not retain the granted flow",
            ));
        }
        peer.shutdown().await.map_err(failure)?;
        let Input::Closed { key, result } =
            tokio::time::timeout(Duration::from_secs(3), fixture.signaling.network.next())
                .await
                .map_err(failure)??
        else {
            return Err(ControlError::Invalid("Expected closed registration flow"));
        };
        result?;
        fixture.signaling.registration_closed(key);
        fixture.signaling.registration_tick(5000).await?;
        if !matches!(
            fixture.signaling.registrations.clients[0].state,
            State::Stopped
        ) {
            return Err(ControlError::Invalid(
                "Disconnected registration kept refreshing",
            ));
        }
        fixture.signaling.registrations.clients[0].state = State::Waiting { at: 5001 };
        let (prepared, mut replacement) = fixture.prepared(5001).await?;
        fixture.signaling.registration_prepared(prepared, 5001)?;
        fixture.signaling.registration_closed(key);
        fixture.signaling.registration_tick(5001).await?;
        let mut decoder = SipStreamDecoder::new();
        let next = receive(&mut replacement, &mut decoder).await?;
        if next.request.cseq_header().map_err(failure)?.value() != "2 REGISTER"
            || next.request.call_id_header().map_err(failure)?.value()
                != initial.request.call_id_header().map_err(failure)?.value()
        {
            return Err(ControlError::Invalid(
                "Old flow changed replacement registration identity",
            ));
        }
        Ok(())
    }
    .await;
    fixture.finish(primary).await
}
