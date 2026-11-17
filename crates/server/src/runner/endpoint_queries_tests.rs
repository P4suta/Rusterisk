use super::{Controller, Runner};
use crate::{control::CallSettings, Action, ControlError, Credentials};
use rusterisk_config::Config;
use rusterisk_core::{ContactPresence, SettingsLifetimes};
use std::time::Duration;

#[path = "endpoint_queries_tests/shutdown.rs"]
mod shutdown;

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

async fn joined(
    mut task: tokio::task::JoinHandle<Result<(), ControlError>>,
) -> Result<(), ControlError> {
    match tokio::time::timeout(Duration::from_secs(5), &mut task).await {
        Ok(result) => result.map_err(failure)?,
        Err(primary) => {
            task.abort();
            match task.await {
                Err(error) if error.is_cancelled() => Err(failure(primary)),
                Err(error) => Err(failure(format!("{primary}; query cleanup failed: {error}"))),
                Ok(Ok(())) => Err(failure(primary)),
                Ok(Err(error)) => Err(failure(format!("{primary}; query cleanup failed: {error}"))),
            }
        }
    }
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

#[tokio::test]
async fn endpoint_observations_reject_old_and_foreign_settings_before_mutation(
) -> Result<(), ControlError> {
    let (controller, mut runner, _effects) = Controller::new(configuration()?, 4);
    let starting = runner.configuration_generation();
    replace(&mut runner)?;
    let current = runner.configuration_generation();
    let foreign = SettingsLifetimes::new(2).current();
    let task = tokio::spawn(runner.run());
    let primary = async {
        for generation in [starting, foreign] {
            if controller
                .observe_endpoint(generation, "phone".into(), ContactPresence::Present)
                .await
                .is_ok()
            {
                return Err(ControlError::Invalid(
                    "Endpoint observation accepted stale settings",
                ));
            }
        }
        let endpoint = controller
            .execute(
                Credentials::new("app".into(), "test-password".into())?,
                None,
                Action::GetEndpoint {
                    technology: "PJSIP".into(),
                    resource: "phone".into(),
                },
            )
            .await?;
        if endpoint.body.ok_or(ControlError::Capacity)?["state"] != "offline" {
            return Err(ControlError::Invalid(
                "Rejected endpoint observations changed availability",
            ));
        }
        controller
            .observe_endpoint(current, "phone".into(), ContactPresence::Present)
            .await?;
        Ok(())
    }
    .await;
    controller.shutdown();
    match (primary, joined(task).await) {
        (primary, Ok(())) => primary,
        (Ok(()), Err(cleanup)) => Err(cleanup),
        (Err(primary), Err(cleanup)) => Err(failure(format!(
            "{primary}; query cleanup failed: {cleanup}"
        ))),
    }
}
