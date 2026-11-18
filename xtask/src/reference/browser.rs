use super::{
    browser_network::BrowserNetwork,
    commands::{arguments, combine, docker, CommandPhase, Commands},
    copy_configuration,
    resources::{reference_ready, reference_versions, start_reference, Reference},
    Interop, ReferenceImage, Step,
};
use crate::process::CommandSupervisor;
use crate::server_distribution::{check_server_ready_log, ExtractedServer, ServerCandidate};
use std::{
    io,
    net::{Ipv4Addr, SocketAddr},
    path::Path,
    time::{Duration, Instant},
};

fn packaged_start(image: &ReferenceImage, plan: &Interop, executable: &Path) -> io::Result<Step> {
    let executable = executable
        .to_str()
        .filter(|path| !path.contains(',') && !path.chars().any(char::is_control))
        .filter(|path| Path::new(path).is_absolute())
        .ok_or_else(|| io::Error::other("packaged browser executable mount is ambiguous"))?;
    let mut step = image.start(plan).map_err(io::Error::other)?;
    let image_index = step
        .arguments
        .iter()
        .position(|argument| argument == image.as_str())
        .ok_or_else(|| io::Error::other("packaged browser server image is missing"))?;
    step.arguments.truncate(image_index);
    step.arguments.extend([
        "--mount".into(),
        format!("type=bind,source={executable},target=/usr/local/bin/rusterisk,readonly"),
        image.as_str().into(),
        "600".into(),
        "/usr/local/bin/rusterisk".into(),
        "--config-dir".into(),
        "/etc/asterisk".into(),
        "--data-dir".into(),
        "/var/spool/asterisk".into(),
    ]);
    Ok(step)
}

fn packaged_ready(
    id: &str,
    commands: &mut Commands<'_>,
    supervisor: &mut CommandSupervisor,
) -> io::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(10);
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, 8088));
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::Error::other(
                "packaged browser server did not report readiness",
            ));
        }
        let output = commands.run(
            docker(&arguments(["logs", id])),
            remaining,
            |command, budget| supervisor.run(command, budget),
        )?;
        if check_server_ready_log(&output.stdout, address)? {
            return Ok(());
        }
        std::thread::sleep(remaining.min(Duration::from_millis(100)));
    }
}

pub(crate) fn with_browser_reference<T>(
    source: &Path,
    state: &Path,
    identity: &str,
    image: &ReferenceImage,
    compare: impl FnOnce(&str) -> io::Result<T>,
) -> io::Result<T> {
    with_browser_target(source, state, identity, image, None, compare)
}

pub(crate) fn with_browser_package<T>(
    source: &Path,
    state: &Path,
    identity: &str,
    image: &ReferenceImage,
    package: &ServerCandidate,
    compare: impl FnOnce(&str) -> io::Result<T>,
) -> io::Result<T> {
    with_browser_target(source, state, identity, image, Some(package), compare)
}

fn with_browser_target<T>(
    source: &Path,
    state: &Path,
    identity: &str,
    image: &ReferenceImage,
    package: Option<&ServerCandidate>,
    compare: impl FnOnce(&str) -> io::Result<T>,
) -> io::Result<T> {
    let (name, example, server) = match package {
        Some(_) => ("browser-package", "examples/browser/config", "Rusterisk"),
        None => (
            "browser-reference",
            "tests/interop/asterisk-browser",
            "Asterisk",
        ),
    };
    let directory = state.join(name);
    std::fs::create_dir(&directory)?;
    let configuration = directory.join("config");
    copy_configuration(&source.join(example), &configuration)?;
    if package.is_some() {
        std::fs::write(
            configuration.join("ari.conf"),
            b"[general]\nenabled=yes\nallowed_origins=http://127.0.0.1:3000\n[app]\ntype=user\npassword=test-password\nread_only=no\n",
        )?;
    }
    std::fs::create_dir(configuration.join("sounds"))?;
    let configuration = configuration
        .to_str()
        .ok_or_else(|| io::Error::other("browser reference configuration path is not UTF-8"))?;
    let mut commands = Commands::new(source, &directory)?;
    let mut supervisor = CommandSupervisor::new().map_err(io::Error::other)?;
    let mut network = BrowserNetwork::new(identity)?;
    let extracted = package
        .map(|package| package.extract(&directory.join("server")))
        .transpose()?;
    let run = (|| {
        let network_id = network.create(&mut commands, &mut supervisor)?;
        let plan =
            Interop::for_browser(identity, configuration, &network_id).map_err(io::Error::other)?;
        let mut reference = Reference::new(identity, directory.join("container.cid"), &plan)?;
        let operation = (|| {
            let id = match &extracted {
                Some(server) => {
                    let step = packaged_start(image, &plan, &server.executable()?)?;
                    reference.launch(image, step, &mut commands, &mut supervisor)?
                }
                None => start_reference(
                    &mut reference,
                    &plan,
                    Some(image),
                    &mut commands,
                    &mut supervisor,
                )?,
            };
            commands
                .run(
                    docker(&arguments([
                        "container",
                        "inspect",
                        "--format",
                        "{{range .NetworkSettings.Networks}}{{.NetworkID}}{{end}}",
                        &id,
                    ])),
                    Duration::from_secs(10),
                    |command, budget| supervisor.run(command, budget),
                )?
                .exact(format!("{network_id}\n").as_bytes())?;
            network.verify(&network_id, &mut commands, &mut supervisor)?;
            match &extracted {
                Some(_) => packaged_ready(&id, &mut commands, &mut supervisor)?,
                None => {
                    reference_versions(&id, &mut commands, &mut supervisor)?;
                    reference_ready(&id, &mut commands, &mut supervisor)?;
                }
            }
            compare(&id)
        })();
        commands.phase = CommandPhase::Cleanup;
        let operation = operation.map_err(|primary| {
            match reference.diagnostics(&plan, &mut commands, &mut supervisor) {
                Ok(logs) => io::Error::other(format!("{primary}; {server} output: {logs}")),
                Err(error) => io::Error::other(format!("{primary}; {server} diagnostics: {error}")),
            }
        });
        combine(
            operation,
            reference
                .cleanup_recorded(&mut commands, &mut supervisor)
                .map(drop),
            "browser reference container cleanup",
        )
    })();
    commands.phase = CommandPhase::Cleanup;
    let operation = combine(
        run,
        network.cleanup(&mut commands, &mut supervisor),
        "browser reference network cleanup",
    );
    let operation = combine(
        operation,
        supervisor.finish().map_err(io::Error::other),
        "browser reference command cleanup",
    );
    combine(
        operation,
        extracted.map_or(Ok(()), ExtractedServer::close),
        "browser package extraction cleanup",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_packaged_server_keeps_the_reference_limits_and_has_one_read_only_executable(
    ) -> io::Result<()> {
        let image = ReferenceImage::parse_inspection(&format!("sha256:{}", "a".repeat(64)))
            .map_err(io::Error::other)?;
        let plan = Interop::for_browser("package", "/config", &"b".repeat(64))
            .map_err(io::Error::other)?;
        let reference = image.start(&plan).map_err(io::Error::other)?;
        let packaged = packaged_start(&image, &plan, Path::new("/owned/bin/rusterisk"))?;
        let before_image = |step: &Step| {
            step.arguments
                .iter()
                .position(|argument| argument == image.as_str())
                .ok_or_else(|| io::Error::other("fixture start image missing"))
        };
        let index = before_image(&reference)?;
        assert_eq!(packaged.arguments[..index], reference.arguments[..index]);
        assert_eq!(
            &packaged.arguments[index..],
            &[
                "--mount",
                "type=bind,source=/owned/bin/rusterisk,target=/usr/local/bin/rusterisk,readonly",
                image.as_str(),
                "600",
                "/usr/local/bin/rusterisk",
                "--config-dir",
                "/etc/asterisk",
                "--data-dir",
                "/var/spool/asterisk",
            ]
        );
        for path in ["relative/executable", "/owned/executable,ambiguous"] {
            assert!(packaged_start(&image, &plan, Path::new(path)).is_err());
        }
        Ok(())
    }
}
