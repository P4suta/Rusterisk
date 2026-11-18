use super::{follow, CommandMode, CoreAssurancePlan, CoreMutationError, Phase};
use crate::{core_assurance::NIGHTLY, process::ProcessError};
use std::{fs, io, path::Path, process::Command, time::Duration};

const COMMIT: &str = "8925ea358a0f265ca61026aadc7ecc506c545cbe";

#[derive(Clone, Copy)]
enum Tool {
    Docker,
    Rustc,
    Cargo,
    Mutants,
}

impl Tool {
    const ALL: [Self; 4] = [Self::Docker, Self::Rustc, Self::Cargo, Self::Mutants];
    fn name(self) -> &'static str {
        match self {
            Self::Docker => "docker",
            Self::Rustc => "rustc",
            Self::Cargo => "cargo",
            Self::Mutants => "mutants",
        }
    }
    fn arguments(self) -> Vec<String> {
        match self {
            Self::Docker => ["--version"].map(str::to_owned).into(),
            Self::Rustc => ["rustc".to_owned(), format!("+{NIGHTLY}"), "-vV".to_owned()].into(),
            Self::Cargo => ["cargo".to_owned(), format!("+{NIGHTLY}"), "-vV".to_owned()].into(),
            Self::Mutants => [
                "cargo".to_owned(),
                format!("+{NIGHTLY}"),
                "mutants".to_owned(),
                "--version".to_owned(),
            ]
            .into(),
        }
    }
}

pub(super) fn files() -> impl Iterator<Item = String> {
    Tool::ALL.into_iter().flat_map(|tool| {
        ["stdout", "stderr"].map(|stream| format!("tools/{}.{stream}.txt", tool.name()))
    })
}

fn invalid() -> CoreMutationError {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "mutation native tool outputs differ from the pinned image profile",
    )
    .into()
}

fn field<'a>(text: &'a str, key: &str) -> Result<&'a str, CoreMutationError> {
    let mut fields = text.lines().filter_map(|line| line.strip_prefix(key));
    let value = fields.next().ok_or_else(invalid)?;
    if value.is_empty() || value.len() > 256 || fields.next().is_some() {
        return Err(invalid());
    }
    Ok(value)
}

fn rust<'a>(text: &'a str, program: &str) -> Result<&'a str, CoreMutationError> {
    let release = field(text, "release: ")?;
    if !release.ends_with("-nightly")
        || semver::Version::parse(release).is_err()
        || !text.lines().next().is_some_and(|line| {
            line.starts_with(&format!("{program} {release} (")) && line.ends_with(')')
        })
    {
        return Err(invalid());
    }
    let commit = field(text, "commit-hash: ")?;
    if commit.len() != 40
        || !commit.bytes().all(|byte| byte.is_ascii_hexdigit())
        || (program == "rustc" && commit != COMMIT)
    {
        return Err(invalid());
    }
    let host = field(text, "host: ")?;
    if !matches!(
        host,
        "x86_64-unknown-linux-gnu" | "aarch64-unknown-linux-gnu"
    ) {
        return Err(invalid());
    }
    Ok(host)
}

pub(super) fn check(state: &Path) -> Result<(), CoreMutationError> {
    let text = |tool: Tool| {
        let bytes = super::read(
            state,
            Path::new(&format!("tools/{}.stdout.txt", tool.name())),
        )?;
        if bytes.len() > 65_536
            || !super::read(
                state,
                Path::new(&format!("tools/{}.stderr.txt", tool.name())),
            )?
            .is_empty()
        {
            return Err(invalid());
        }
        String::from_utf8(bytes).map_err(|_| invalid())
    };
    let docker = text(Tool::Docker)?;
    let (version, build) = docker
        .trim_end_matches('\n')
        .strip_prefix("Docker version ")
        .and_then(|line| line.split_once(", build "))
        .ok_or_else(invalid)?;
    if semver::Version::parse(version).is_err()
        || build.is_empty()
        || build.chars().any(char::is_whitespace)
    {
        return Err(invalid());
    }
    let rustc = text(Tool::Rustc)?;
    let cargo = text(Tool::Cargo)?;
    if rust(&rustc, "rustc")? != rust(&cargo, "cargo")?
        || field(&rustc, "release: ")? != field(&cargo, "release: ")?
        || text(Tool::Mutants)? != "cargo-mutants 27.1.0\n"
    {
        return Err(invalid());
    }
    Ok(())
}

fn command(tool: Tool, plan: &CoreAssurancePlan) -> Result<Command, CoreMutationError> {
    let mut command = Command::new("docker");
    match tool {
        Tool::Docker => {
            command.args(tool.arguments());
        }
        Tool::Rustc | Tool::Cargo | Tool::Mutants => {
            let mut arguments = plan.step(Phase::MutationInventory).arguments;
            let index = arguments
                .iter()
                .position(|argument| argument == "cargo")
                .ok_or(CoreMutationError::Plan)?;
            arguments.truncate(index);
            let timeout = arguments.last_mut().ok_or(CoreMutationError::Plan)?;
            if timeout != "880s" {
                return Err(CoreMutationError::Plan);
            }
            "10s".clone_into(timeout);
            command.args(arguments).args(tool.arguments());
        }
    }
    Ok(command)
}

pub(super) fn capture(
    state: &Path,
    plan: &CoreAssurancePlan,
    perform: &mut impl FnMut(&mut Command, Duration, CommandMode) -> Result<(), ProcessError>,
) -> Result<(), CoreMutationError> {
    fs::create_dir(state.join("tools"))?;
    for tool in Tool::ALL {
        let mut command = command(tool, plan)?;
        let output = |stream| {
            fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(state.join(format!("tools/{}.{stream}.txt", tool.name())))
        };
        command.stdout(output("stdout")?).stderr(output("stderr")?);
        let result = perform(&mut command, Duration::from_secs(15), CommandMode::Work);
        let cleanup = match (&result, tool) {
            (Ok(()) | Err(ProcessError::Status(_)), _) | (_, Tool::Docker) => Ok(()),
            (
                Err(
                    ProcessError::Io(_)
                    | ProcessError::Timeout
                    | ProcessError::InvalidBudget
                    | ProcessError::InvalidOutput
                    | ProcessError::Interrupted
                    | ProcessError::UnsupportedSupervision
                    | ProcessError::Cleanup { .. },
                ),
                _,
            ) => {
                let step = plan.cleanup(Phase::MutationInventory);
                perform(
                    Command::new("docker").args(step.arguments),
                    step.budget,
                    CommandMode::Cleanup,
                )
            }
        };
        follow(
            result.map_err(CoreMutationError::Process),
            cleanup.map_err(CoreMutationError::Process),
            "mutation native tool container cleanup failed",
        )?;
    }
    check(state)
}
