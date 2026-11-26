use rusterisk_api::{Channel, ChannelId, ExternalRtpTarget, StaticRtpCodec};
use rusterisk_config::Config;
use rusterisk_server::{Action, ControlError, Controller, Credentials, Effect, Signal};
use rusterisk_test_support::Wait;
use std::{
    future::{poll_fn, Future},
    task::Poll,
};

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}
fn credentials() -> Result<Credentials, ControlError> {
    Credentials::new("app".into(), "test-password".into())
}
fn id(name: &str) -> Result<ChannelId, ControlError> {
    Ok(ChannelId::try_from(name.to_owned())?)
}
fn create(name: &str) -> Result<Action, ControlError> {
    Ok(Action::ExternalRtp {
        id: id(name)?,
        app: "calls".into(),
        target: ExternalRtpTarget::new(
            "127.0.0.1:12000".parse().map_err(failure)?,
            StaticRtpCodec::Pcmu,
        )
        .map_err(failure)?,
        data: String::new(),
    })
}

#[tokio::test]
async fn abandoned_external_requests_release_their_generation_and_reject_late_completion(
) -> Result<(), ControlError> {
    let configuration = Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=test-password\n",
        "",
        "",
        "",
    )
    .map_err(failure)?;
    let (controller, runner, mut effects) = Controller::new(configuration, 4);
    let task = tokio::spawn(runner.run());
    let result = async {
        let _subscription = controller
            .subscribe(credentials()?, None, "calls".into())
            .await?;
        let abandoned = {
            let mut request =
                std::pin::pin!(controller.execute(credentials()?, None, create("reused")?));
            assert!(
                poll_fn(|context| Poll::Ready(request.as_mut().poll(context).is_pending())).await
            );
            let Effect::ConnectExternalRtp { key, .. } =
                effects.recv().await.ok_or(ControlError::Capacity)?
            else {
                return Err(ControlError::Unsupported);
            };
            assert!(
                poll_fn(|context| Poll::Ready(request.as_mut().poll(context).is_pending())).await
            );
            key
        };
        let released = Wait::Ready
            .bounded(effects.recv())
            .await
            .map_err(failure)?
            .ok_or(ControlError::Capacity)?;
        assert!(matches!(released, Effect::Release { key } if key == abandoned));
        let mut replacement =
            std::pin::pin!(controller.execute(credentials()?, None, create("reused")?));
        assert!(
            poll_fn(|context| Poll::Ready(replacement.as_mut().poll(context).is_pending())).await
        );
        let Effect::ConnectExternalRtp { key: current, .. } =
            effects.recv().await.ok_or(ControlError::Capacity)?
        else {
            return Err(ControlError::Unsupported);
        };
        assert_ne!(current.generation, abandoned.generation);
        controller
            .signal(Signal::ExternalRtpAdmitted {
                key: abandoned.clone(),
                result: Ok("127.0.0.1:10000".parse().map_err(failure)?),
            })
            .await?;
        let released = Wait::Ready
            .bounded(effects.recv())
            .await
            .map_err(failure)?
            .ok_or(ControlError::Capacity)?;
        assert!(matches!(released, Effect::Release { key } if key == abandoned));
        assert!(
            poll_fn(|context| Poll::Ready(replacement.as_mut().poll(context).is_pending())).await
        );
        controller
            .signal(Signal::ExternalRtpAdmitted {
                key: current,
                result: Ok("127.0.0.1:10002".parse().map_err(failure)?),
            })
            .await?;
        let response = Wait::Ready.bounded(replacement).await.map_err(failure)??;
        let channel: Channel =
            serde_json::from_value(response.body.ok_or(ControlError::Capacity)?)?;
        assert_eq!(
            channel
                .channelvars
                .as_ref()
                .and_then(|variables| variables.get("UNICASTRTP_LOCAL_PORT"))
                .map(String::as_str),
            Some("10002")
        );
        Ok(())
    }
    .await;
    controller.shutdown();
    task.await.map_err(failure)??;
    result
}

#[tokio::test]
async fn external_replies_wait_for_transport_variables_while_other_requests_progress(
) -> Result<(), ControlError> {
    let configuration = Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=test-password\n",
        "",
        "",
        "",
    )
    .map_err(failure)?;
    let (controller, runner, mut effects) = Controller::new(configuration, 4);
    let task = tokio::spawn(runner.run());
    let result = async {
        let _subscription = controller
            .subscribe(credentials()?, None, "calls".into())
            .await?;
        let mut request =
            std::pin::pin!(controller.execute(credentials()?, None, create("accepted")?));
        assert!(poll_fn(|context| Poll::Ready(request.as_mut().poll(context).is_pending())).await);
        let Effect::ConnectExternalRtp { key, .. } =
            effects.recv().await.ok_or(ControlError::Capacity)?
        else {
            return Err(ControlError::Unsupported);
        };
        assert!(poll_fn(|context| Poll::Ready(request.as_mut().poll(context).is_pending())).await);
        assert_eq!(
            controller
                .execute(credentials()?, None, Action::ListChannels)
                .await?
                .status,
            200
        );
        controller
            .signal(Signal::ExternalRtpAdmitted {
                key,
                result: Ok("127.0.0.1:10000".parse().map_err(failure)?),
            })
            .await?;
        let response = Wait::Ready.bounded(request).await.map_err(failure)??;
        let channel: Channel =
            serde_json::from_value(response.body.ok_or(ControlError::Capacity)?)?;
        assert_eq!(
            channel
                .channelvars
                .as_ref()
                .and_then(|variables| variables.get("UNICASTRTP_LOCAL_PORT"))
                .map(String::as_str),
            Some("10000")
        );
        let mut rejected =
            std::pin::pin!(controller.execute(credentials()?, None, create("rejected")?));
        assert!(poll_fn(|context| Poll::Ready(rejected.as_mut().poll(context).is_pending())).await);
        let Effect::ConnectExternalRtp { key, .. } =
            effects.recv().await.ok_or(ControlError::Capacity)?
        else {
            return Err(ControlError::Unsupported);
        };
        controller
            .signal(Signal::ExternalRtpAdmitted {
                key: key.clone(),
                result: Err(ControlError::Capacity),
            })
            .await?;
        assert!(matches!(
            Wait::Ready.bounded(rejected).await.map_err(failure)?,
            Err(ControlError::Capacity)
        ));
        let released = effects.recv().await.ok_or(ControlError::Capacity)?;
        assert!(matches!(released, Effect::Release { key: current } if current == key));
        assert!(matches!(
            controller
                .execute(credentials()?, None, Action::GetChannel(id("rejected")?))
                .await,
            Err(ControlError::NotFound(_))
        ));
        Ok(())
    }
    .await;
    controller.shutdown();
    task.await.map_err(failure)??;
    result
}
