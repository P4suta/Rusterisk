use super::{
    browser_network::BrowserNetwork,
    commands::{arguments, combine, docker, CommandPhase, Commands},
    copy_configuration,
    resources::{reference_ready, reference_versions, start_reference, Reference},
    Interop, ReferenceImage,
};
use crate::process::CommandSupervisor;
use std::{io, path::Path, time::Duration};

pub(crate) fn with_browser_reference<T>(
    source: &Path,
    state: &Path,
    identity: &str,
    image: &ReferenceImage,
    compare: impl FnOnce(&str) -> io::Result<T>,
) -> io::Result<T> {
    let directory = state.join("browser-reference");
    std::fs::create_dir(&directory)?;
    let configuration = directory.join("config");
    copy_configuration(
        &source.join("tests/interop/asterisk-browser"),
        &configuration,
    )?;
    std::fs::create_dir(configuration.join("sounds"))?;
    let configuration = configuration
        .to_str()
        .ok_or_else(|| io::Error::other("browser reference configuration path is not UTF-8"))?;
    let mut commands = Commands::new(source, &directory)?;
    let mut supervisor = CommandSupervisor::new().map_err(io::Error::other)?;
    let mut network = BrowserNetwork::new(identity)?;
    let run = (|| {
        let network_id = network.create(&mut commands, &mut supervisor)?;
        let plan =
            Interop::for_browser(identity, configuration, &network_id).map_err(io::Error::other)?;
        let mut reference = Reference::new(identity, directory.join("container.cid"), &plan)?;
        let operation = (|| {
            let id = start_reference(
                &mut reference,
                &plan,
                Some(image),
                &mut commands,
                &mut supervisor,
            )?;
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
            reference_versions(&id, &mut commands, &mut supervisor)?;
            reference_ready(&id, &mut commands, &mut supervisor)?;
            compare(&id)
        })();
        commands.phase = CommandPhase::Cleanup;
        let operation = operation.map_err(|primary| {
            match reference.diagnostics(&plan, &mut commands, &mut supervisor) {
                Ok(logs) => io::Error::other(format!("{primary}; Asterisk output: {logs}")),
                Err(error) => io::Error::other(format!("{primary}; Asterisk diagnostics: {error}")),
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
    combine(
        operation,
        supervisor.finish().map_err(io::Error::other),
        "browser reference command cleanup",
    )
}
