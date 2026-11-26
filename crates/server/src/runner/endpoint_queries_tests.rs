use super::{Controller, Runner};
use crate::{control::CallSettings, Action, ControlError};
use rusterisk_api::{Endpoint, EndpointState};
use rusterisk_config::Config;
use rusterisk_core::{ContactPresence, SettingsLifetimes};

#[path = "endpoint_queries_tests/contact_publication.rs"]
mod contact_publication;

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}

fn configuration() -> Result<Config, ControlError> {
    Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=test-password\n",
        "[udp]\ntype=transport\nprotocol=udp\nbind=127.0.0.1:5060\n[auth]\ntype=auth\nusername=phone\npassword=test\n[phone]\ntype=aor\nmax_contacts=1\n[phone]\ntype=endpoint\ntransport=udp\nauth=auth\naors=phone\nallow=ulaw\n",
        "",
        "",
    )
    .map_err(failure)
}

fn replace(runner: &mut Runner) -> Result<(), ControlError> {
    let replacement = configuration()?;
    let settings = CallSettings::configured(&replacement);
    let candidate = runner
        .control
        .prepare_configuration(replacement, settings)?;
    runner.control.commit_configuration(candidate)?;
    Ok(())
}

fn state(runner: &mut Runner) -> Result<EndpointState, ControlError> {
    let outcome = runner.control.apply(
        Action::GetEndpoint {
            technology: "PJSIP".into(),
            resource: "phone".into(),
        },
        "endpoint observation",
        0,
    )?;
    let endpoint: Endpoint = serde_json::from_value(outcome.body.ok_or(ControlError::Capacity)?)?;
    endpoint
        .state
        .ok_or(ControlError::Invalid("Endpoint state missing"))
}

#[test]
fn endpoint_observations_reject_old_and_foreign_settings_before_mutation(
) -> Result<(), ControlError> {
    let (_controller, mut runner, _effects) = Controller::new(configuration()?, 4);
    let starting = runner.configuration_generation();
    replace(&mut runner)?;
    let current = runner.configuration_generation();
    let foreign = SettingsLifetimes::new(2).current();
    for generation in [starting, foreign] {
        let refused =
            runner.endpoint_observation(generation, "phone".into(), ContactPresence::Present, 0)?;
        assert!(refused.is_err());
    }
    assert_eq!(state(&mut runner)?, EndpointState::Offline);
    runner.endpoint_observation(current, "phone".into(), ContactPresence::Present, 0)??;
    assert_eq!(state(&mut runner)?, EndpointState::Online);
    Ok(())
}
