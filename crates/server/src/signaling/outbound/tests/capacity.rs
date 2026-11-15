use super::{failure, Config, ControlError, Controller, MediaHub, Signaling};
use crate::{CallKey, Runner};
use rusterisk_api::ChannelId;

struct Fixture {
    signaling: Signaling,
    runner: Runner,
    hub: MediaHub,
}

impl Fixture {
    async fn new() -> Result<Self, ControlError> {
        let config = Config::parse("[general]\nbindaddr=127.0.0.1\n", "[app]\ntype=user\npassword=test\n", "[stream]\ntype=transport\nprotocol=tcp\nbind=127.0.0.1:0\n[auth]\ntype=auth\nusername=phone\npassword=password\n[static]\ntype=aor\ncontact=sip:phone@127.0.0.1:5070\n[phone]\ntype=endpoint\ntransport=stream\nauth=auth\naors=static\nallow=ulaw\n", "", "").map_err(failure)?;
        let (controller, runner, effects) = Controller::new(config.clone(), 64);
        let (media, hub, notices) = MediaHub::new(64);
        let signaling = Signaling::bind(config, controller, effects, media, notices).await?;
        Ok(Self {
            signaling,
            runner,
            hub,
        })
    }

    async fn finish(mut self) -> Result<(), ControlError> {
        self.signaling.finish_native().await?;
        self.signaling.network.close().await?;
        drop((self.runner, self.hub));
        Ok(())
    }
}

fn key(id: &str, generation: u64) -> Result<CallKey, ControlError> {
    Ok(CallKey {
        id: ChannelId::try_from(id.to_owned())?,
        generation,
    })
}

#[tokio::test]
async fn cancelling_jobs_keep_admission_capacity_until_their_completion_is_joined(
) -> Result<(), ControlError> {
    let mut fixture = Fixture::new().await?;
    for generation in 1..=16 {
        fixture.signaling.queue_native(
            key(&format!("pending-{generation}"), generation)?,
            "PJSIP/phone".into(),
            "100".into(),
            Some(30),
            0,
        )?;
    }
    for generation in 1..=16 {
        fixture
            .signaling
            .cancel_native(&key(&format!("pending-{generation}"), generation)?);
    }
    assert!(fixture.signaling.native_pending.is_empty());
    assert!(matches!(
        fixture.signaling.queue_native(
            key("replacement", 17)?,
            "PJSIP/phone".into(),
            "100".into(),
            Some(30),
            0
        ),
        Err(ControlError::Capacity)
    ));
    while let Some(completed) = fixture.signaling.native_jobs.join_next().await {
        fixture
            .signaling
            .native_completed(completed.map_err(failure)?, 1)
            .await?;
    }
    fixture.signaling.queue_native(
        key("replacement", 17)?,
        "PJSIP/phone".into(),
        "100".into(),
        Some(30),
        1,
    )?;
    assert_eq!(fixture.signaling.native_pending.len(), 1);
    fixture.finish().await
}

#[tokio::test]
async fn delayed_resolution_completions_cannot_touch_a_reused_call_identifier(
) -> Result<(), ControlError> {
    let mut fixture = Fixture::new().await?;
    let previous = key("reused", 1)?;
    let current = key("reused", 2)?;
    fixture.signaling.queue_native(
        previous.clone(),
        "PJSIP/phone".into(),
        "100".into(),
        Some(30),
        0,
    )?;
    fixture.signaling.cancel_native(&previous);
    let completed = fixture
        .signaling
        .native_jobs
        .join_next()
        .await
        .ok_or(ControlError::Capacity)?
        .map_err(failure)?;
    fixture.signaling.queue_native(
        current.clone(),
        "PJSIP/phone".into(),
        "100".into(),
        Some(30),
        1,
    )?;
    fixture.signaling.native_completed(completed, 1).await?;
    assert!(!fixture.signaling.native_pending.contains_key(&previous));
    assert!(fixture.signaling.native_pending.contains_key(&current));
    assert!(fixture.signaling.calls.is_empty());
    fixture.finish().await
}
