use rusterisk_client::{
    api::ChannelState, CallMilestone, Client, DtmfDigit, DtmfTiming, Error, MuteDirection,
    Originate,
};
use rusterisk_server::{Effect, Milestone, Signal};
use std::{collections::BTreeMap, future::IntoFuture, time::Duration};

enum AudioControl {
    Hold,
    Mute,
    Unhold,
    Unmute,
}

#[tokio::test]
async fn owned_calls_wait_for_distinct_milestones_without_blocking_events_and_cancel_on_drop(
) -> Result<(), Error> {
    let config = rusterisk_config::Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=test-password\n",
        "",
        "",
        "",
    )
    .map_err(|_| Error::Invalid("test configuration"))?;
    let (controller, runner, mut effects) = rusterisk_server::Controller::new(config, 4);
    let runner = tokio::spawn(runner.run());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(
        axum::serve(listener, rusterisk_server::http::router(controller.clone())).into_future(),
    );
    let result = async {
        let client = Client::new(&format!("http://{address}"), "app", "test-password")?;
        let mut session = client.subscribe(&"app".to_owned().try_into()?).await?;
        for variables in [
            BTreeMap::from([
                ("VALID".into(), "first".into()),
                (String::new(), "refused".into()),
            ]),
            (0..65)
                .map(|index| (format!("V{index}"), "value".into()))
                .collect(),
        ] {
            let mut invalid = Originate::new("PJSIP/phone".into())?;
            invalid.variables = variables;
            assert!(matches!(
                session.originate(invalid).await,
                Err(Error::Invalid(_))
            ));
            assert!(effects.try_recv().is_err());
            assert!(client.channels().await?.is_empty());
        }
        let mut oversized = Originate::new("PJSIP/phone".into())?;
        oversized.variables = (0..5)
            .map(|index| (format!("V{index}"), "x".repeat(4096)))
            .collect();
        assert!(matches!(
            session.originate(oversized).await,
            Err(Error::Capacity)
        ));
        assert!(effects.try_recv().is_err());
        let mut options = Originate::new("PJSIP/phone".into())?;
        options
            .variables
            .insert("INITIAL_TAG".into(), "value /&=test".into());
        options
            .variables
            .insert("CALLERID(name)".into(), "Alice".into());
        let mut call = session.originate(options).await?;
        let accepted = call
            .wait(CallMilestone::Accepted, Duration::from_secs(2))
            .await?;
        assert_eq!(
            accepted.channel.as_ref().map(|channel| channel.state),
            Some(ChannelState::Down)
        );
        assert!(!accepted.progress.has_reached(CallMilestone::Stasis));
        assert_eq!(
            accepted
                .channel
                .as_ref()
                .map(|channel| channel.caller.name.as_str()),
            Some("Alice")
        );
        assert_eq!(
            client.variable(call.id(), "INITIAL_TAG").await?.value,
            "value /&=test"
        );
        let effect = tokio::time::timeout(Duration::from_secs(2), effects.recv())
            .await
            .map_err(|_| Error::Timeout)?
            .ok_or(Error::Closed)?;
        let Effect::Start { key, .. } = effect else {
            return Err(Error::Invalid("expected start effect"));
        };
        controller
            .signal(Signal::Milestone {
                key: key.clone(),
                milestone: Milestone::Answered,
            })
            .await
            .map_err(|_| Error::Closed)?;
        call.wait(CallMilestone::Stasis, Duration::from_secs(2))
            .await?;
        call.wait(CallMilestone::Answered, Duration::from_secs(2))
            .await?;
        loop {
            let event = tokio::time::timeout(Duration::from_secs(2), session.next_event())
                .await
                .map_err(|_| Error::Timeout)??;
            if let rusterisk_client::api::EventKind::StasisStart { args, .. } = event.kind {
                assert!(
                    args.is_empty(),
                    "omitted arguments must stay empty: {args:?}"
                );
                break;
            }
        }
        assert!(matches!(
            call.wait(CallMilestone::MediaConnected, Duration::from_millis(30))
                .await,
            Err(Error::Timeout)
        ));
        controller
            .signal(Signal::Milestone {
                key: key.clone(),
                milestone: Milestone::MediaConnected,
            })
            .await
            .map_err(|_| Error::Closed)?;
        call.wait(CallMilestone::MediaConnected, Duration::from_secs(2))
            .await?;
        client
            .set_variable(call.id(), "ACCOUNT_TAG", Some("value /&=test"))
            .await?;
        assert_eq!(
            client.variable(call.id(), "ACCOUNT_TAG").await?.value,
            "value /&=test"
        );
        client.set_variable(call.id(), "ACCOUNT_TAG", None).await?;
        assert!(matches!(
            client.variable(call.id(), "ACCOUNT_TAG").await,
            Err(Error::Api { status: 404, .. })
        ));
        for (operation, gate) in [
            (AudioControl::Hold, rusterisk_core::AudioGate::Closed),
            (AudioControl::Mute, rusterisk_core::AudioGate::Closed),
            (
                AudioControl::Unhold,
                rusterisk_core::AudioGate::IncomingMuted,
            ),
            (AudioControl::Unmute, rusterisk_core::AudioGate::Open),
        ] {
            match operation {
                AudioControl::Hold => client.hold(call.id()).await?,
                AudioControl::Mute => client.mute(call.id(), MuteDirection::Incoming).await?,
                AudioControl::Unhold => client.unhold(call.id()).await?,
                AudioControl::Unmute => client.unmute(call.id(), MuteDirection::Incoming).await?,
            }
            let effect = tokio::time::timeout(Duration::from_secs(2), effects.recv())
                .await
                .map_err(|_| Error::Timeout)?
                .ok_or(Error::Closed)?;
            assert!(matches!(effect, Effect::Gate { gate: actual, .. } if actual == gate));
        }
        controller
            .signal(Signal::DtmfReady {
                key: key.clone(),
                events: u16::MAX,
            })
            .await
            .map_err(|_| Error::Closed)?;
        client
            .send_dtmf(
                call.id(),
                &[DtmfDigit::Five, DtmfDigit::Hash],
                DtmfTiming::new(3, 20, 60, 4).map_err(|_| Error::Invalid("test timing"))?,
            )
            .await?;
        let effect = tokio::time::timeout(Duration::from_secs(2), effects.recv())
            .await
            .map_err(|_| Error::Timeout)?
            .ok_or(Error::Closed)?;
        let Effect::Dtmf { mut command, .. } = effect else {
            return Err(Error::Invalid("expected digit effect"));
        };
        assert!(command
            .sequence
            .advance(0)
            .map_err(|_| Error::Invalid("test clock"))?
            .is_none());
        assert_eq!(
            command
                .sequence
                .advance(3)
                .map_err(|_| Error::Invalid("test clock"))?
                .map(|tone| (tone.digit, tone.duration_ms)),
            Some((DtmfDigit::Five, 60))
        );
        assert!(command
            .sequence
            .advance(82)
            .map_err(|_| Error::Invalid("test clock"))?
            .is_none());
        assert_eq!(
            command
                .sequence
                .advance(83)
                .map_err(|_| Error::Invalid("test clock"))?
                .map(|tone| (tone.digit, tone.duration_ms)),
            Some((DtmfDigit::Hash, 60))
        );
        assert!(command
            .sequence
            .advance(146)
            .map_err(|_| Error::Invalid("test clock"))?
            .is_none());
        assert!(!command.sequence.finished());
        assert!(command
            .sequence
            .advance(147)
            .map_err(|_| Error::Invalid("test clock"))?
            .is_none());
        assert!(command.sequence.finished());
        let id = call.id().clone();
        drop(call);
        let released = tokio::time::timeout(Duration::from_secs(2), effects.recv())
            .await
            .map_err(|_| Error::Timeout)?
            .ok_or(Error::Closed)?;
        assert!(matches!(released, Effect::Release { key: released } if released == key));
        assert!(matches!(
            client.channel(&id).await,
            Err(Error::Api { status: 404, .. })
        ));
        session.shutdown().await?;
        Ok(())
    }
    .await;
    controller.shutdown();
    runner
        .await
        .map_err(|_| Error::Closed)?
        .map_err(|_| Error::Closed)?;
    server.abort();
    assert!(server.await.is_err());
    result
}
