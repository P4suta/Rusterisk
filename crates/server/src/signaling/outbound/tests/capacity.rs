use super::{failure, Config, ControlError, Controller, MediaHub, Signaling};
use crate::{CallKey, Runner};
use rsipstack::sip::{
    headers::{
        auth::{Algorithm, AuthQop},
        typed::WwwAuthenticate,
    },
    services::DigestGenerator,
    Method, SipMessage, Uri,
};
use rusterisk_api::ChannelId;
use std::num::NonZeroU64;

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
        let primary = self.signaling.finish_native().await;
        self.signaling.native_jobs.abort_all();
        let mut cleanup = Ok(());
        while let Some(completed) = self.signaling.native_jobs.join_next().await {
            let result = match completed {
                Ok(completed) => self.signaling.native_completed(completed, 1).await,
                Err(error) if error.is_cancelled() => Ok(()),
                Err(error) => Err(failure(error)),
            };
            cleanup = crate::signaling::cleanup::combine(cleanup, result);
        }
        cleanup = crate::signaling::cleanup::combine(cleanup, self.signaling.network.close().await);
        drop((self.runner, self.hub));
        crate::signaling::cleanup::combine(primary, cleanup)
    }
}

#[tokio::test]
async fn native_shutdown_drains_every_failed_job_and_recovers_known_setup_slots(
) -> Result<(), ControlError> {
    let mut fixture = Fixture::new().await?;
    let primary: Result<(), ControlError> = async {
        for generation in 1..=16 {
            fixture.signaling.queue_native(
                key(&format!("aborted-{generation}"), generation)?,
                "PJSIP/phone".into(),
                "100".into(),
                0,
            )?;
        }
        fixture.signaling.native_jobs.abort_all();
        if fixture.signaling.finish_native().await.is_ok() {
            return Err(ControlError::Invalid("Native shutdown lost task failures"));
        }
        if !fixture.signaling.native_jobs.is_empty() || !fixture.signaling.native_pending.is_empty()
        {
            return Err(ControlError::Invalid(
                "Native shutdown left failed jobs unjoined",
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
    let cleanup = fixture.finish().await;
    crate::signaling::cleanup::combine(primary, cleanup)
}

async fn registered_fixture() -> Result<Fixture, ControlError> {
    let config = Config::parse("[general]\nbindaddr=127.0.0.1\n", "[app]\ntype=user\npassword=test\n", "[udp]\ntype=transport\nprotocol=udp\nbind=127.0.0.1:0\n[auth]\ntype=auth\nusername=phone\npassword=password\n[registered]\ntype=aor\nmax_contacts=1\n[phone]\ntype=endpoint\ntransport=udp\nauth=auth\naors=registered\nallow=ulaw\n", "", "").map_err(failure)?;
    let (controller, runner, effects) = Controller::new(config.clone(), 64);
    let (media, hub, notices) = MediaHub::new(64);
    let mut signaling = Signaling::bind(config, controller, effects, media, notices).await?;
    let local = signaling.network.local_addr(0)?;
    let peer = "127.0.0.2:5070".parse().map_err(failure)?;
    let uri = Uri::try_from(format!("sip:{local}")).map_err(failure)?;
    let register = |sequence, authorization| {
        format!("REGISTER {uri} SIP/2.0\r\nVia: SIP/2.0/UDP {peer};branch=z9hG4bK-capacity-register-{sequence};rport\r\nFrom: <sip:phone@localhost>;tag=phone\r\nTo: <sip:registered@localhost>\r\nCall-ID: registered-capacity\r\nCSeq: {sequence} REGISTER\r\nContact: <sip:phone@{peer};transport=udp>\r\nExpires: 120\r\nMax-Forwards: 70\r\n{authorization}Content-Length: 0\r\n\r\n")
    };
    let output = signaling.listeners[0]
        .engine
        .receive(register(1, "").as_bytes(), peer, 0)
        .map_err(failure)?;
    let packet = output.datagrams.first().ok_or(ControlError::Capacity)?;
    let SipMessage::Response(response) =
        SipMessage::try_from(packet.bytes.as_slice()).map_err(failure)?
    else {
        return Err(ControlError::Invalid("Expected registration challenge"));
    };
    let challenge = response
        .headers
        .iter()
        .find(|header| {
            header.name().eq_ignore_ascii_case("WWW-Authenticate")
                && header.value().contains("algorithm=MD5")
        })
        .ok_or(ControlError::Invalid("Missing MD5 registration challenge"))?;
    let challenge = WwwAuthenticate::parse(challenge.value()).map_err(failure)?;
    let qop = AuthQop::Auth {
        cnonce: "capacity-client".into(),
        nc: 1,
    };
    let digest = DigestGenerator {
        username: "phone",
        password: "password",
        nonce: &challenge.nonce,
        uri: &uri,
        realm: &challenge.realm,
        method: &Method::Register,
        qop: Some(&qop),
        algorithm: Algorithm::Md5,
    }
    .compute();
    let authorization = format!("Authorization: Digest username=\"phone\", realm=\"{}\", nonce=\"{}\", uri=\"{uri}\", response=\"{digest}\", algorithm=MD5, qop=auth, cnonce=\"capacity-client\", nc=00000001\r\n", challenge.realm, challenge.nonce);
    let output = signaling.listeners[0]
        .engine
        .receive(register(2, &authorization).as_bytes(), peer, 0)
        .map_err(failure)?;
    let packet = output.datagrams.first().ok_or(ControlError::Capacity)?;
    let SipMessage::Response(response) =
        SipMessage::try_from(packet.bytes.as_slice()).map_err(failure)?
    else {
        return Err(ControlError::Invalid("Expected registration response"));
    };
    if response.status_code.code() != 200 {
        return Err(ControlError::Invalid(
            "Registration did not establish the call route",
        ));
    }
    Ok(Fixture {
        signaling,
        runner,
        hub,
    })
}

fn key(id: &str, generation: u64) -> Result<CallKey, ControlError> {
    Ok(CallKey {
        id: ChannelId::try_from(id.to_owned())?,
        generation,
    })
}

#[tokio::test]
async fn cancelled_registered_calls_keep_the_shared_setup_slots_until_their_jobs_are_joined(
) -> Result<(), ControlError> {
    let mut fixture = registered_fixture().await?;
    let primary: Result<(), ControlError> = async {
        for generation in 1..=16 {
            fixture.signaling.queue_native(
                key(&format!("registered-{generation}"), generation)?,
                "PJSIP/phone".into(),
                "100".into(),
                0,
            )?;
        }
        for generation in 1..=16 {
            fixture
                .signaling
                .cancel_native(&key(&format!("registered-{generation}"), generation)?);
        }
        if !fixture.signaling.native_pending.is_empty()
            || !matches!(
                fixture.signaling.network.reserve_setup(0, NonZeroU64::MIN),
                Err(ControlError::Capacity)
            )
        {
            return Err(ControlError::Invalid(
                "Cancelled registered calls released shared setup capacity",
            ));
        }
        while let Some(completed) = fixture.signaling.native_jobs.join_next().await {
            fixture
                .signaling
                .native_completed(completed.map_err(failure)?, 1)
                .await?;
        }
        let recovered = fixture
            .signaling
            .network
            .reserve_setup(0, NonZeroU64::MIN)?;
        fixture.signaling.network.finish_resolution(recovered);
        Ok(())
    }
    .await;
    let cleanup = fixture.finish().await;
    crate::signaling::cleanup::combine(primary, cleanup)
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
    fixture
        .signaling
        .queue_native(previous.clone(), "PJSIP/phone".into(), "100".into(), 0)?;
    fixture.signaling.cancel_native(&previous);
    let completed = fixture
        .signaling
        .native_jobs
        .join_next()
        .await
        .ok_or(ControlError::Capacity)?
        .map_err(failure)?;
    fixture
        .signaling
        .queue_native(current.clone(), "PJSIP/phone".into(), "100".into(), 1)?;
    fixture.signaling.native_completed(completed, 1).await?;
    assert!(!fixture.signaling.native_pending.contains_key(&previous));
    assert!(fixture.signaling.native_pending.contains_key(&current));
    assert!(fixture.signaling.calls.is_empty());
    fixture.finish().await
}
