use super::Request;
use crate::{Action, ControlError, Controller, Credentials, Effect, IncomingCall, IncomingTarget};
use rusterisk_api::ChannelId;
use rusterisk_config::Config;
use rusterisk_core::Wake;
use std::{future::Future, pin::Pin, time::Duration};
use tokio::{sync::oneshot, time::Instant};

const OFFSET: Duration = Duration::from_millis(37);

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}

fn configuration() -> Result<Config, ControlError> {
    Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=test\n",
        "[udp]\ntype=transport\nprotocol=udp\nbind=127.0.0.1:5060\n[auth]\ntype=auth\nusername=phone\npassword=test\n[phone]\ntype=aor\ncontact=sip:phone@127.0.0.1:5061\n[phone]\ntype=endpoint\ntransport=udp\nauth=auth\naors=phone\nallow=ulaw\n",
        "",
        "[internal]\nexten => 200,1,Wait(0.25)\nsame => n,Hangup()\n",
    )
    .map_err(failure)
}

fn credentials() -> Result<Credentials, ControlError> {
    Credentials::new("app".into(), "test".into())
}

async fn progress<F>(serving: &mut Pin<&mut F>) -> Result<(), ControlError>
where
    F: Future<Output = Result<(), ControlError>>,
{
    for _ in 0..16 {
        tokio::select! {
            biased;
            served = &mut *serving => return Err(failure(format!("the runner stopped: {served:?}"))),
            () = tokio::task::yield_now() => {}
        }
    }
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn an_idle_runner_takes_no_turn_for_an_hour() -> Result<(), ControlError> {
    let (_controller, mut runner, _effects) = Controller::new(configuration()?, 4);
    let mut polls = 0_u32;
    {
        let mut serving = std::pin::pin!(runner.serve());
        let counted = std::future::poll_fn(|context| {
            polls += 1;
            serving.as_mut().poll(context)
        });
        tokio::select! {
            biased;
            () = tokio::time::sleep(Duration::from_secs(3600)) => {}
            served = counted => return Err(failure(format!("the idle runner stopped: {served:?}"))),
        }
    }
    assert_eq!(polls, 1, "an idle runner woke without an event");
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn an_originate_timeout_fires_at_its_instant() -> Result<(), ControlError> {
    let (controller, mut runner, mut effects) = Controller::new(configuration()?, 4);
    runner.control.subscribe("app")?;
    let task = tokio::spawn(runner.run());
    tokio::time::sleep(OFFSET).await;
    let begun = Instant::now();
    controller
        .execute(
            credentials()?,
            None,
            Action::Originate {
                id: ChannelId::try_from("timed".to_owned())?,
                endpoint: "PJSIP/phone".into(),
                app: "app".into(),
                args: Vec::new(),
                caller: "100".into(),
                timeout: 1,
            },
        )
        .await?;
    let Some(Effect::Start { key, .. }) = effects.recv().await else {
        return Err(ControlError::Invalid("Expected the call to start"));
    };
    let released = tokio::time::timeout(Duration::from_secs(5), effects.recv()).await;
    let waited = begun.elapsed();
    controller.shutdown();
    task.await.map_err(failure)??;
    assert!(matches!(released, Ok(Some(Effect::Release { key: ended })) if ended == key));
    assert_eq!(waited, Duration::from_secs(1));
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn a_dialplan_wait_ends_at_its_instant() -> Result<(), ControlError> {
    let (controller, runner, mut effects) = Controller::new(configuration()?, 4);
    let task = tokio::spawn(runner.run());
    tokio::time::sleep(OFFSET).await;
    let begun = Instant::now();
    let key = controller
        .incoming(IncomingCall {
            id: ChannelId::try_from("waiting".to_owned())?,
            endpoint: "phone".into(),
            caller: "100".into(),
            context: "internal".into(),
            extension: "200".into(),
            target: IncomingTarget::Dialplan,
        })
        .await?;
    let released = tokio::time::timeout(Duration::from_secs(5), effects.recv()).await;
    let waited = begun.elapsed();
    controller.shutdown();
    task.await.map_err(failure)??;
    assert!(matches!(released, Ok(Some(Effect::Release { key: ended })) if ended == key));
    assert_eq!(waited, Duration::from_millis(250));
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn a_dropped_subscription_ends_without_a_free_request_slot_or_a_timer(
) -> Result<(), ControlError> {
    let (controller, mut runner, _effects) = Controller::new(configuration()?, 4);
    {
        let mut serving = std::pin::pin!(runner.serve());
        let subscription = tokio::select! {
            biased;
            subscription = controller.subscribe(credentials()?, None, "app".into()) => subscription?,
            served = &mut serving => return Err(failure(format!("the runner stopped: {served:?}"))),
        };
        let filler = || -> Result<Request, ControlError> {
            Ok(Request::Authorize {
                credentials: credentials()?,
                origin: None,
                read_only: true,
                reply: oneshot::channel().0,
            })
        };
        while controller.requests.try_send(filler()?).is_ok() {}
        drop(subscription);
        progress(&mut serving).await?;
    }
    assert!(runner.subscribers.is_empty());
    assert!(runner.sessions.is_empty());
    Ok(())
}

async fn abandoned<F: Future>(request: F) {
    let mut request = std::pin::pin!(request);
    let pending = std::future::poll_fn(|context| {
        std::task::Poll::Ready(request.as_mut().poll(context).is_pending())
    })
    .await;
    assert!(pending, "the request ended before its requester left");
}

#[tokio::test(start_paused = true)]
async fn an_abandoned_request_wakes_the_runner() -> Result<(), ControlError> {
    let (controller, mut runner, _effects) = Controller::new(configuration()?, 4);
    abandoned(controller.subscribe(credentials()?, None, "app".into())).await;
    assert!(runner.closed.try_recv().is_ok());
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn an_abandoned_reload_requester_wakes_the_runner() -> Result<(), ControlError> {
    let (controller, mut runner, _effects) = Controller::new(configuration()?, 4);
    abandoned(controller.reload(configuration()?)).await;
    assert!(runner.closed.try_recv().is_ok());
    Ok(())
}

#[tokio::test]
async fn a_recording_admission_needs_a_wake_at_its_deadline_and_none_after_it(
) -> Result<(), ControlError> {
    let (_controller, mut runner, _effects) = Controller::new(configuration()?, 4);
    assert_eq!(runner.wake()?, Wake::Idle);
    runner.control.subscribe("app")?;
    runner.control.apply(
        Action::CreateBridge {
            id: ChannelId::try_from("mix".to_owned())?,
            name: String::new(),
        },
        "now",
        0,
    )?;
    let (reply, mut response) = oneshot::channel();
    runner.request(
        Request::Apply {
            credentials: credentials()?,
            origin: None,
            action: Action::RecordBridge {
                bridge: ChannelId::try_from("mix".to_owned())?,
                name: ChannelId::try_from("saved".to_owned())?,
                options: Default::default(),
            },
            variables: Default::default(),
            reply,
        },
        100,
    )?;
    assert_eq!(runner.wake()?, Wake::At(5100));
    runner.settle(5099)?;
    assert!(response.try_recv().is_err());
    assert_eq!(runner.wake()?, Wake::At(5100));
    runner.settle(5100)?;
    assert!(matches!(
        response.try_recv(),
        Ok(Err(ControlError::Capacity))
    ));
    assert_eq!(runner.wake()?, Wake::Idle);
    Ok(())
}
