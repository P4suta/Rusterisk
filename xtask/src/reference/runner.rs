use super::ReferenceImage;
#[cfg(unix)]
use super::{
    callback_address,
    commands::{combine, docker, owned_step, CommandPhase, Commands},
    media_address, native_server_build, published_address,
    resources::{reference_ready, reference_versions, start_reference, Reference},
    retained::{CaseContext, CaseRun, CaseRuntime, ReferenceNativeContext},
    ReferenceCase, SippAdapter,
};
#[cfg(any(unix, test))]
use super::{copy_configuration, prepare_music_fixture, Interop};
use crate::process::{CommandSupervisor, ProcessError};
use std::path::{Path, PathBuf};
#[cfg(any(unix, test))]
use std::{
    fs,
    io::{self, Write},
};
#[cfg(unix)]
use std::{
    process::Command,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[cfg(test)]
#[path = "runner_configuration_tests.rs"]
mod configuration_tests;

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

#[cfg(unix)]
struct CaseExecution<'a> {
    source: &'a Path,
    image: &'a ReferenceImage,
    retained: bool,
}

impl ReferenceRunner {
    #[cfg(any(unix, test))]
    pub(crate) fn prepare_configuration(source: &Path, destination: &Path) -> io::Result<()> {
        copy_configuration(source, destination)?;
        let operation = (|| {
            // Asterisk rechecks wildcard includes even when the outer file metadata matches.
            for name in ["pjsip", "extensions"] {
                let path = destination.join(format!("{name}.conf"));
                fs::File::create_new(destination.join(format!("{name}-scenario-fixture.conf")))?
                    .write_all(&fs::read(&path)?)?;
                fs::write(path, format!("#include {name}-scenario-*.conf\n"))?;
            }
            let music = prepare_music_fixture(destination)?;
            fs::create_dir(destination.join("sounds"))?;
            for (relative, bytes) in music.native_files() {
                let path = destination.join("native").join(relative);
                fs::create_dir_all(
                    path.parent()
                        .ok_or_else(|| io::Error::other("native music parent missing"))?,
                )?;
                let mut file = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(path)?;
                file.write_all(bytes)?;
                file.flush()?;
            }
            Ok(())
        })();
        match operation {
            Ok(()) => Ok(()),
            Err(primary) => match fs::remove_dir_all(destination) {
                Ok(()) => Err(primary),
                Err(cleanup) => Err(io::Error::other(format!(
                    "{primary}; reference configuration cleanup failed: {cleanup}"
                ))),
            },
        }
    }

    #[cfg(any(unix, test))]
    pub(crate) fn configuration_path(source: &Path, owner: &str) -> io::Result<PathBuf> {
        if !source.is_absolute()
            || source.components().any(|component| {
                matches!(
                    component,
                    std::path::Component::CurDir | std::path::Component::ParentDir
                )
            })
        {
            return Err(io::Error::other(
                "reference source must be an absolute path",
            ));
        }
        let configuration = source
            .join("target/interop")
            .join(format!("{owner}.configuration"));
        Interop::new(
            owner,
            configuration
                .to_str()
                .ok_or_else(|| io::Error::other("reference configuration path is not UTF-8"))?,
        )
        .map_err(io::Error::other)?;
        Ok(configuration)
    }

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
        self.compare_recorded(source, image, false).map(|_| ())
    }

    #[cfg(unix)]
    pub(crate) fn compare_retained(
        &mut self,
        source: &Path,
        image: &ReferenceImage,
    ) -> Result<ReferenceNativeContext, ProcessError> {
        self.compare_recorded(source, image, true)
    }

    #[cfg(unix)]
    fn compare_recorded(
        &mut self,
        source: &Path,
        image: &ReferenceImage,
        retained: bool,
    ) -> Result<ReferenceNativeContext, ProcessError> {
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
        let executable = source.join("target/debug/rusterisk");
        let comparison = if retained {
            image.compare_sipp_retained_with(
                source,
                &executable,
                &mut self.supervisor,
                &self.state.join("sipp"),
            )?
        } else {
            image.compare_sipp_registrations_with(source, &executable, &mut self.supervisor)?
        };
        println!(
            "SIPp completed calls: Asterisk={}, Rusterisk={}",
            comparison.completed_calls(SippAdapter::Asterisk),
            comparison.completed_calls(SippAdapter::Rusterisk),
        );
        let sipp = comparison.into_context();
        let cases = self.compare_cases(
            &CaseExecution {
                source,
                image,
                retained,
            },
            &mut commands,
        )?;
        Ok(ReferenceNativeContext { sipp, cases })
    }

    #[cfg(unix)]
    fn compare_cases(
        &mut self,
        execution: &CaseExecution<'_>,
        commands: &mut Commands<'_>,
    ) -> io::Result<CaseContext> {
        let source = execution.source;
        let image = execution.image;
        let owner = identity()?;
        let configuration = Self::configuration_path(source, &owner)?;
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
            fs::create_dir_all(source.join("target/interop"))?;
            Self::prepare_configuration(&source.join("tests/interop/asterisk"), &configuration)?;
            if execution.retained {
                super::retained::preserve_initial(&configuration, commands.directory)?;
            }
            let id = start_reference(
                &mut reference,
                &plan,
                Some(image),
                commands,
                &mut self.supervisor,
            )?;
            let tools = reference_versions(&id, commands, &mut self.supervisor)?;
            reference_ready(&id, commands, &mut self.supervisor)?;
            let mut inspect = |step| -> io::Result<(String, u8)> {
                let step = owned_step(step, reference.name(), &id);
                let observed =
                    commands.run(docker(&step.arguments), step.budget, |command, budget| {
                        self.supervisor.run(command, budget)
                    })?;
                Ok((observed.text()?.into(), commands.last_output()))
            };
            let (text, address_output) = inspect(plan.port())?;
            let address = published_address(&text).map_err(io::Error::other)?;
            let (text, media_output) = inspect(plan.media_address())?;
            let media = media_address(&text).map_err(io::Error::other)?;
            let (text, callback_output) = inspect(plan.callback_address())?;
            let callback = callback_address(&text).map_err(io::Error::other)?;
            let mut cases = [0; ReferenceCase::ALL.len()];
            for (case, index) in ReferenceCase::ALL.into_iter().zip(&mut cases) {
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
                *index = commands.last_output();
            }
            let runtime = if execution.retained {
                Some(CaseRuntime::capture(source, &configuration, callback)?)
            } else {
                None
            };
            Ok(CaseRun {
                owner,
                configuration,
                id,
                address,
                media,
                callback,
                address_output,
                media_output,
                callback_output,
                tools,
                cases,
                runtime,
            })
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
                (Ok(run), Ok(_)) => Ok(run),
            }
        } else {
            operation
        };
        match (
            operation,
            reference.cleanup_recorded(commands, &mut self.supervisor),
        ) {
            (Ok(run), Ok(Some(cleanup))) => Ok(CaseContext { run, cleanup }),
            (Ok(_), Ok(None)) => Err(io::Error::other(
                "completed reference cases have no owned cleanup",
            )),
            (Err(primary), Ok(_)) | (Ok(_), Err(primary)) => Err(primary),
            (Err(primary), Err(cleanup)) => Err(io::Error::other(format!(
                "{primary}; reference cleanup: {cleanup}"
            ))),
        }
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
