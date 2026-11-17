use super::{configuration, failure};
use crate::{
    control::CallSettings,
    media::MediaHub,
    reload::PublicationPort,
    signaling::{NativePreparedSignaling, Signaling},
    Action, ControlError, Controller, Runner, Signal,
};
use rusterisk_api::{Endpoint, EndpointState};
use rusterisk_config::Config;
use rusterisk_core::ContactPresence;
use rusterisk_sip::{Output, SipEvent};
use std::{task::Poll, time::Duration};
use tokio::sync::mpsc::error::TryRecvError;

#[derive(Clone, Copy)]
enum Observation {
    Scoped,
    Legacy,
}

fn require(condition: bool, message: &'static str) -> Result<(), ControlError> {
    condition
        .then_some(())
        .ok_or(ControlError::Invalid(message))
}

fn combine(
    primary: Result<(), ControlError>,
    cleanup: Result<(), ControlError>,
) -> Result<(), ControlError> {
    match (primary, cleanup) {
        (primary, Ok(())) => primary,
        (Ok(()), Err(error)) => Err(error),
        (Err(primary), Err(cleanup)) => Err(failure(format!(
            "{primary}; endpoint publication fixture cleanup failed: {cleanup}"
        ))),
    }
}

fn requests(runner: &mut Runner) -> Result<(), ControlError> {
    for _ in 0..128 {
        match runner.requests.try_recv() {
            Ok(request) => runner.request(request, 0)?,
            Err(TryRecvError::Empty) => return Ok(()),
            Err(TryRecvError::Disconnected) => {
                return Err(ControlError::Invalid("Endpoint request scope ended"))
            }
        }
    }
    Err(ControlError::Capacity)
}

fn signals(runner: &mut Runner) -> Result<(), ControlError> {
    for _ in 0..128 {
        match runner.signals.try_recv() {
            Ok(signal) => runner.apply_signal(signal, 0)?,
            Err(TryRecvError::Empty) => return Ok(()),
            Err(TryRecvError::Disconnected) => {
                return Err(ControlError::Invalid("Endpoint signal scope ended"))
            }
        }
    }
    Err(ControlError::Capacity)
}

fn state(runner: &mut Runner) -> Result<EndpointState, ControlError> {
    let outcome = runner.control.apply(
        Action::GetEndpoint {
            technology: "PJSIP".into(),
            resource: "phone".into(),
        },
        "endpoint publication observation",
        0,
    )?;
    let endpoint: Endpoint = serde_json::from_value(outcome.body.ok_or(ControlError::Capacity)?)?;
    endpoint
        .state
        .ok_or(ControlError::Invalid("Endpoint state missing"))
}

struct Fixture {
    config: Config,
    controller: Controller,
    runner: Runner,
    signaling: Signaling,
    publication: Option<PublicationPort<NativePreparedSignaling>>,
}

impl Fixture {
    async fn new(observation: Observation) -> Result<Self, ControlError> {
        let mut config = configuration()?;
        config
            .transports
            .first_mut()
            .ok_or(ControlError::Capacity)?
            .bind
            .set_port(0);
        let (controller, runner, effects) = Controller::new(config.clone(), 4);
        let (media, _hub, notices) = MediaHub::new(4);
        let signaling =
            Signaling::bind(config.clone(), controller.clone(), effects, media, notices).await?;
        let mut runner = runner
            .with_native_profiles(signaling.native_profiles())?
            .with_websocket_profiles(signaling.websocket_profiles())?;
        requests(&mut runner)?;
        let (signaling, publication) = match observation {
            Observation::Scoped => {
                let (port, receiver) = PublicationPort::channel();
                let generation = runner.configuration_generation();
                (signaling.with_publication(receiver, generation), Some(port))
            }
            Observation::Legacy => (signaling, None),
        };
        Ok(Self {
            config,
            controller,
            runner,
            signaling,
            publication,
        })
    }

    async fn output(&mut self, presence: ContactPresence) -> Result<(), ControlError> {
        let key = self.runner.control.endpoint_key("phone")?;
        let output = Output {
            datagrams: Vec::new(),
            events: [SipEvent::EndpointContact {
                endpoint: key.resource,
                generation: key.generation,
                presence,
            }]
            .into_iter()
            .collect(),
        };
        let output = self.signaling.output_for_test(0, output, 0);
        tokio::pin!(output);
        for _ in 0..128 {
            match futures_util::poll!(output.as_mut()) {
                Poll::Ready(result) => return result,
                Poll::Pending => {
                    let request = self
                        .runner
                        .requests
                        .recv()
                        .await
                        .ok_or(ControlError::Invalid("Endpoint request scope ended"))?;
                    self.runner.request(request, 0)?;
                }
            }
        }
        Err(ControlError::Capacity)
    }

    fn finish(
        self,
        primary: Result<(), ControlError>,
    ) -> impl std::future::Future<Output = Result<(), ControlError>> {
        let addresses = self.signaling.listeners();
        let stopped = self.controller.shutdown_signal();
        self.controller.shutdown();
        let Self {
            signaling,
            runner,
            controller,
            publication,
            ..
        } = self;
        drop((runner, controller, publication));
        async move {
            let cleanup = tokio::time::timeout(Duration::from_secs(3), signaling.run(stopped))
                .await
                .map_err(failure)
                .and_then(std::convert::identity);
            let mut result = combine(primary, cleanup);
            match addresses {
                Ok(addresses) => {
                    for (_, address) in addresses {
                        result = combine(
                            result,
                            tokio::net::UdpSocket::bind(address)
                                .await
                                .map(drop)
                                .map_err(failure),
                        );
                    }
                }
                Err(error) => result = combine(result, Err(error)),
            }
            result
        }
    }
}

async fn exercise(observation: Observation, presence: ContactPresence) -> Result<(), ControlError> {
    let mut fixture = Fixture::new(observation).await?;
    let primary = tokio::time::timeout(Duration::from_secs(5), async {
        let previous = match presence {
            ContactPresence::Present => ContactPresence::Absent,
            ContactPresence::Absent => ContactPresence::Present,
        };
        let key = fixture.runner.control.endpoint_key("phone")?;
        fixture.runner.apply_signal(
            Signal::EndpointContact {
                key,
                presence: previous,
            },
            0,
        )?;
        let candidate = fixture.runner.control.prepare_configuration(
            fixture.config.clone(),
            CallSettings::configured(&fixture.config),
        )?;
        fixture.output(presence).await?;
        match observation {
            Observation::Scoped => {}
            Observation::Legacy => signals(&mut fixture.runner)?,
        }
        fixture.runner.control.commit_configuration(candidate)?;
        signals(&mut fixture.runner)?;
        let expected = match presence {
            ContactPresence::Present => EndpointState::Online,
            ContactPresence::Absent => EndpointState::Offline,
        };
        require(
            state(&mut fixture.runner)? == expected,
            "Prepared commit lost a completed native contact observation",
        )
    })
    .await
    .map_err(failure)
    .and_then(std::convert::identity);
    fixture.finish(primary).await
}

#[tokio::test]
async fn scoped_present_observation_survives_a_prepared_commit() -> Result<(), ControlError> {
    exercise(Observation::Scoped, ContactPresence::Present).await
}

#[tokio::test]
async fn scoped_absent_observation_survives_a_prepared_commit() -> Result<(), ControlError> {
    exercise(Observation::Scoped, ContactPresence::Absent).await
}

#[tokio::test]
async fn legacy_present_signal_survives_a_prepared_commit() -> Result<(), ControlError> {
    exercise(Observation::Legacy, ContactPresence::Present).await
}

#[tokio::test]
async fn legacy_absent_signal_survives_a_prepared_commit() -> Result<(), ControlError> {
    exercise(Observation::Legacy, ContactPresence::Absent).await
}
