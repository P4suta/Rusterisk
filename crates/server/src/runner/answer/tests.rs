use super::{Controller, Credentials, Runner};
use crate::{
    CallKey, ControlError, Effect, IncomingCall, IncomingTarget, Milestone, Outcome, Signal,
};
use rusterisk_api::ChannelId;
use rusterisk_config::Config;
use tokio::sync::{mpsc, oneshot};

struct Fixture {
    runner: Runner,
    effects: mpsc::Receiver<Effect>,
    credentials: Credentials,
    key: CallKey,
}

impl Fixture {
    fn new() -> Result<Self, ControlError> {
        let config = Config::parse(
            "[general]\nbindaddr=127.0.0.1",
            "[app]\ntype=user\npassword=test",
            "",
            "",
            "",
        )
        .map_err(|error| ControlError::Adapter(error.to_string()))?;
        let (_, mut runner, effects) = Controller::new(config, 4);
        runner.control.subscribe("calls")?;
        let id = ChannelId::try_from("incoming".to_owned())?;
        runner.control.incoming(
            IncomingCall {
                id: id.clone(),
                endpoint: "phone".into(),
                caller: "100".into(),
                context: "internal".into(),
                extension: "100".into(),
                target: IncomingTarget::Stasis {
                    application: "calls".into(),
                    arguments: Vec::new(),
                },
            },
            "now",
        )?;
        let key = runner.control.key(&id)?;
        Ok(Self {
            runner,
            effects,
            credentials: Credentials::new("app".into(), "test".into())?,
            key,
        })
    }

    fn request(&mut self, now: u64) -> oneshot::Receiver<Result<Outcome, ControlError>> {
        let (reply, receiver) = oneshot::channel();
        self.runner.request_answer(
            self.credentials.clone(),
            None,
            self.key.id.clone(),
            reply,
            now,
        );
        receiver
    }
}

#[test]
fn disconnecting_the_last_waiter_keeps_one_signaling_command_until_its_deadline(
) -> Result<(), ControlError> {
    let mut fixture = Fixture::new()?;
    drop(fixture.request(0));
    assert!(matches!(fixture.effects.try_recv(), Ok(Effect::Answer { key }) if key == fixture.key));
    fixture.runner.finish_answers(1);
    let mut retried = fixture.request(2);
    assert!(matches!(
        fixture.effects.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));
    fixture.runner.finish_answers(5001);
    assert!(matches!(
        retried.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    ));
    fixture.runner.finish_answers(5002);
    assert!(matches!(
        retried.try_recv(),
        Ok(Err(ControlError::Capacity))
    ));
    let mut last = fixture.request(5003);
    assert!(matches!(fixture.effects.try_recv(), Ok(Effect::Answer { key }) if key == fixture.key));
    fixture.runner.control.signal(
        Signal::Milestone {
            key: fixture.key.clone(),
            milestone: Milestone::Answered,
        },
        "later",
    )?;
    fixture.runner.finish_answers(5004);
    assert!(matches!(last.try_recv(), Ok(Ok(_))));
    assert!(fixture.runner.answers.is_empty());
    Ok(())
}

#[test]
fn each_waiter_owns_its_timeout_and_disconnected_waiters_release_capacity(
) -> Result<(), ControlError> {
    let mut fixture = Fixture::new()?;
    let mut first = fixture.request(0);
    let waiters: Vec<_> = (0..63).map(|_| fixture.request(1)).collect();
    assert!(matches!(
        fixture.request(2).try_recv(),
        Ok(Err(ControlError::Capacity))
    ));
    drop(waiters);
    fixture.runner.finish_answers(3);
    let mut last = fixture.request(4);
    fixture.runner.finish_answers(5000);
    assert!(matches!(first.try_recv(), Ok(Err(ControlError::Capacity))));
    assert!(matches!(
        last.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    ));
    fixture.runner.finish_answers(5004);
    assert!(matches!(last.try_recv(), Ok(Err(ControlError::Capacity))));
    assert!(fixture.runner.answers.is_empty());
    assert!(matches!(
        fixture.effects.try_recv(),
        Ok(Effect::Answer { .. })
    ));
    assert!(matches!(
        fixture.effects.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));
    Ok(())
}
