use super::ReferenceImage;
#[cfg(unix)]
use super::{
    callback_address,
    commands::{combine, docker, owned_step, CommandPhase, Commands},
    copy_configuration, media_address, native_server_build, published_address,
    resources::{reference_ready, reference_versions, start_reference, Reference},
    Interop, ReferenceCase, SippAdapter,
};
use crate::process::{CommandSupervisor, ProcessError};
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::{
    fs, io,
    process::Command,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[cfg(unix)]
fn identity() -> io::Result<String> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    Ok(format!("{}-{stamp}", std::process::id()))
}

#[cfg(unix)]
fn mise(arguments: &[String]) -> Command {
    let mut command = Command::new("mise");
    command.args(["x", "--"]).args(arguments);
    command
}

pub(crate) struct ReferenceRunner {
    state: PathBuf,
    supervisor: CommandSupervisor,
}

impl ReferenceRunner {
    pub(crate) fn new(state: PathBuf) -> Result<Self, ProcessError> {
        Ok(Self {
            state,
            supervisor: CommandSupervisor::new()?,
        })
    }

    #[cfg(unix)]
    pub(crate) fn inspect_image(
        &mut self,
        source: &Path,
        image: &ReferenceImage,
    ) -> Result<String, ProcessError> {
        let directory = self.state.join("reference-image");
        fs::create_dir(&directory)?;
        let mut commands = Commands::new(source, &directory)?;
        let mut step = ReferenceImage::inspection();
        let selected = step
            .arguments
            .last_mut()
            .ok_or(ProcessError::InvalidOutput)?;
        *selected = image.as_str().into();
        let observed = commands.run(docker(&step.arguments), step.budget, |command, budget| {
            self.supervisor.run(command, budget)
        })?;
        Ok(observed.text()?.into())
    }

    #[cfg(unix)]
    pub(crate) fn compare(
        &mut self,
        source: &Path,
        image: &ReferenceImage,
    ) -> Result<(), ProcessError> {
        let directory = self.state.join("reference-comparisons");
        fs::create_dir(&directory)?;
        let mut commands = Commands::new(source, &directory)?;
        println!("reference preflight: maintained node-check");
        commands.run(
            mise(&["just".into(), "node-check".into()]),
            Duration::from_secs(540),
            |command, budget| self.supervisor.run(command, budget),
        )?;
        let build = native_server_build();
        println!(
            "reference native build: cargo {}",
            build.arguments.join(" ")
        );
        let mut arguments = Vec::with_capacity(build.arguments.len() + 1);
        arguments.push("cargo".into());
        arguments.extend(build.arguments);
        commands.run(mise(&arguments), build.budget, |command, budget| {
            self.supervisor.run(command, budget)
        })?;
        let comparison = image.compare_sipp_registrations_with(
            source,
            &source.join("target/debug/rusterisk"),
            &mut self.supervisor,
        )?;
        println!(
            "SIPp completed calls: Asterisk={}, Rusterisk={}",
            comparison.completed_calls(SippAdapter::Asterisk),
            comparison.completed_calls(SippAdapter::Rusterisk),
        );
        self.compare_cases(source, image, &mut commands)?;
        Ok(())
    }

    #[cfg(unix)]
    fn compare_cases(
        &mut self,
        source: &Path,
        image: &ReferenceImage,
        commands: &mut Commands<'_>,
    ) -> io::Result<()> {
        let configuration = commands.directory.join("asterisk");
        let owner = identity()?;
        let plan = Interop::new(
            &owner,
            configuration
                .to_str()
                .ok_or_else(|| io::Error::other("reference configuration path is not UTF-8"))?,
        )
        .map_err(io::Error::other)?;
        let mut reference =
            Reference::new(&owner, commands.directory.join("reference.cid"), &plan)?;
        let operation = (|| {
            copy_configuration(&source.join("tests/interop/asterisk"), &configuration)?;
            fs::create_dir(configuration.join("sounds"))?;
            let id = start_reference(
                &mut reference,
                &plan,
                Some(image),
                commands,
                &mut self.supervisor,
            )?;
            reference_versions(&id, commands, &mut self.supervisor)?;
            reference_ready(&id, commands, &mut self.supervisor)?;
            let mut inspect = |step| -> io::Result<String> {
                let step = owned_step(step, reference.name(), &id);
                let observed =
                    commands.run(docker(&step.arguments), step.budget, |command, budget| {
                        self.supervisor.run(command, budget)
                    })?;
                Ok(observed.text()?.into())
            };
            let address = published_address(&inspect(plan.port())?).map_err(io::Error::other)?;
            let media = media_address(&inspect(plan.media_address())?).map_err(io::Error::other)?;
            let callback =
                callback_address(&inspect(plan.callback_address())?).map_err(io::Error::other)?;
            for case in ReferenceCase::ALL {
                println!("reference case: {}", case.name());
                let mut arguments = Vec::with_capacity(12);
                arguments.push("cargo".into());
                arguments.extend(case.cargo_arguments().map(str::to_owned));
                let mut command = mise(&arguments);
                command
                    .env("RUSTERISK_REFERENCE_URL", format!("http://{address}"))
                    .env("RUSTERISK_REFERENCE_MEDIA_IP", media.to_string())
                    .env("RUSTERISK_REFERENCE_CALLBACK_IP", callback.to_string())
                    .env("RUSTERISK_REFERENCE_CONFIGURATION", &configuration);
                let observed = commands.run(command, case.budget(), |command, budget| {
                    self.supervisor.run(command, budget)
                })?;
                let output = std::str::from_utf8(&observed.stdout).map_err(io::Error::other)?;
                print!("{output}");
                case.validate_execution(output).map_err(|error| {
                    io::Error::other(format!(
                        "{}: {error}; {}",
                        case.name(),
                        observed.diagnostics()
                    ))
                })?;
            }
            Ok(())
        })();
        commands.phase = CommandPhase::Cleanup;
        let operation = if operation.is_err() {
            match (
                operation,
                reference.diagnostics(&plan, commands, &mut self.supervisor),
            ) {
                (Err(error), Ok(diagnostics)) => Err(io::Error::other(format!(
                    "{error}; reference diagnostics: {diagnostics}"
                ))),
                (operation, Err(error)) => combine(operation, Err(error), "reference diagnostics"),
                (Ok(()), Ok(_)) => Ok(()),
            }
        } else {
            operation
        };
        combine(
            operation,
            reference.cleanup(commands, &mut self.supervisor),
            "reference cleanup",
        )
    }

    pub(crate) fn finish(self) -> Result<(), ProcessError> {
        self.supervisor.finish()
    }

    #[cfg(not(unix))]
    pub(crate) fn inspect_image(
        &mut self,
        source: &Path,
        image: &ReferenceImage,
    ) -> Result<String, ProcessError> {
        let _ = (&self.state, source, image);
        Err(ProcessError::UnsupportedSupervision)
    }

    #[cfg(not(unix))]
    pub(crate) fn compare(
        &mut self,
        source: &Path,
        image: &ReferenceImage,
    ) -> Result<(), ProcessError> {
        let _ = (&self.state, source, image);
        Err(ProcessError::UnsupportedSupervision)
    }
}

#[cfg(unix)]
impl ReferenceImage {
    pub fn inspect(source: &Path) -> io::Result<Self> {
        let directory = fs::canonicalize(std::env::temp_dir())?
            .join(format!("rusterisk-reference-image-{}", identity()?));
        fs::create_dir(&directory)?;
        let operation = (|| {
            let mut supervisor = CommandSupervisor::new().map_err(io::Error::other)?;
            let operation = (|| {
                let mut commands = Commands::new(source, &directory)?;
                let step = Self::inspection();
                let observed =
                    commands.run(docker(&step.arguments), step.budget, |command, budget| {
                        supervisor.run(command, budget)
                    })?;
                Self::parse_inspection(observed.text()?).map_err(io::Error::other)
            })();
            combine(
                operation,
                supervisor.finish().map_err(io::Error::other),
                "reference image supervisor cleanup",
            )
        })();
        combine(
            operation,
            fs::remove_dir_all(directory),
            "reference image state cleanup",
        )
    }
}
