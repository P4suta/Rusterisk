use super::{container_result, CommandMode, CoreAssurancePlan, CoreMiriError, Phase};
use crate::{core_assurance::NIGHTLY, process::ProcessError};
use std::{fs, io, path::Path, process::Command, time::Duration};

const RUSTC_COMMIT: &str = "8925ea358a0f265ca61026aadc7ecc506c545cbe";

#[derive(Clone, Copy)]
enum Tool {
    Docker,
    Rustc,
    Cargo,
    Miri,
}

impl Tool {
    const ALL: [Self; 4] = [Self::Docker, Self::Rustc, Self::Cargo, Self::Miri];

    fn name(self) -> &'static str {
        match self {
            Self::Docker => "docker",
            Self::Rustc => "rustc",
            Self::Cargo => "cargo",
            Self::Miri => "miri",
        }
    }

    fn arguments(self) -> Vec<String> {
        match self {
            Self::Docker => ["--version"].map(str::to_owned).into(),
            Self::Rustc => ["rustc".to_owned(), format!("+{NIGHTLY}"), "-vV".to_owned()].into(),
            Self::Cargo => ["cargo".to_owned(), format!("+{NIGHTLY}"), "-vV".to_owned()].into(),
            Self::Miri => [
                "cargo".to_owned(),
                format!("+{NIGHTLY}"),
                "miri".to_owned(),
                "--version".to_owned(),
            ]
            .into(),
        }
    }
}

fn invalid() -> CoreMiriError {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "Miri native tool outputs differ from the captured nightly image profile",
    )
    .into()
}

pub(super) fn check_recipe(bytes: &[u8]) -> Result<(), CoreMiriError> {
    let source = std::str::from_utf8(bytes).map_err(|_| invalid())?;
    if !source.contains(&format!("rustup toolchain install {NIGHTLY} "))
        || !source.contains(&format!("rustc +{NIGHTLY} -vV"))
        || !source.contains(&format!("'commit-hash: {RUSTC_COMMIT}'"))
    {
        return Err(invalid());
    }
    Ok(())
}

pub(super) fn files() -> impl Iterator<Item = String> {
    Tool::ALL.into_iter().flat_map(|tool| {
        ["stdout", "stderr"].map(|stream| format!("tools/{}.{stream}.txt", tool.name()))
    })
}

fn field<'a>(text: &'a str, key: &str) -> Result<&'a str, CoreMiriError> {
    let mut fields = text.lines().filter_map(|line| line.strip_prefix(key));
    let field = fields.next().ok_or_else(invalid)?;
    if field.is_empty() || field.len() > 256 || fields.next().is_some() {
        return Err(invalid());
    }
    Ok(field)
}

fn hash(text: &str) -> bool {
    (7..=40).contains(&text.len()) && text.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn date(text: &str) -> bool {
    text.len() == 10
        && text.split('-').map(str::len).eq([4, 2, 2])
        && text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'-')
}

fn banner<'a>(text: &'a str, program: &str) -> Result<&'a str, CoreMiriError> {
    let line = text.lines().next().ok_or_else(invalid)?;
    let version = line
        .strip_prefix(program)
        .and_then(|line| line.strip_prefix(' '))
        .ok_or_else(invalid)?;
    let (version, revision) = version.split_once(" (").ok_or_else(invalid)?;
    semver::Version::parse(version).map_err(|_| invalid())?;
    let revision = revision.strip_suffix(')').ok_or_else(invalid)?;
    let (commit, released) = revision.split_once(' ').ok_or_else(invalid)?;
    if !hash(commit) || !date(released) {
        return Err(invalid());
    }
    Ok(version)
}

fn rust(text: &str, program: &str) -> Result<(), CoreMiriError> {
    let version = banner(text, program)?;
    if !version.ends_with("-nightly")
        || field(text, "release: ")? != version
        || !matches!(
            field(text, "host: ")?,
            "x86_64-unknown-linux-gnu" | "aarch64-unknown-linux-gnu"
        )
        || !date(field(text, "commit-date: ")?)
    {
        return Err(invalid());
    }
    let commit = field(text, "commit-hash: ")?;
    if commit.len() != 40 || !hash(commit) || (program == "rustc" && commit != RUSTC_COMMIT) {
        return Err(invalid());
    }
    Ok(())
}

fn single(text: &str) -> Result<&str, CoreMiriError> {
    let mut lines = text.lines();
    let line = lines.next().ok_or_else(invalid)?;
    if line.is_empty() || lines.next().is_some() {
        return Err(invalid());
    }
    Ok(line)
}

pub(super) fn check(state: &Path) -> Result<(), CoreMiriError> {
    let text = |tool: Tool| {
        let bytes = super::read(&state.join(format!("tools/{}.stdout.txt", tool.name())))?;
        if bytes.len() > 65_536
            || !super::read(&state.join(format!("tools/{}.stderr.txt", tool.name())))?.is_empty()
        {
            return Err(invalid());
        }
        String::from_utf8(bytes).map_err(|_| invalid())
    };
    let docker = text(Tool::Docker)?;
    let docker = single(&docker)?
        .strip_prefix("Docker version ")
        .and_then(|line| line.split_once(", build "))
        .ok_or_else(invalid)?;
    semver::Version::parse(docker.0).map_err(|_| invalid())?;
    if docker.1.is_empty() || docker.1.chars().any(char::is_whitespace) {
        return Err(invalid());
    }
    let rustc = text(Tool::Rustc)?;
    let cargo = text(Tool::Cargo)?;
    rust(&rustc, "rustc")?;
    rust(&cargo, "cargo")?;
    if field(&rustc, "host: ")? != field(&cargo, "host: ")?
        || field(&rustc, "release: ")? != field(&cargo, "release: ")?
    {
        return Err(invalid());
    }
    let miri = text(Tool::Miri)?;
    let miri = single(&miri)?;
    if miri.contains(" (") {
        banner(miri, "miri")?;
    } else {
        semver::Version::parse(miri.strip_prefix("miri ").ok_or_else(invalid)?)
            .map_err(|_| invalid())?;
    }
    Ok(())
}

fn command(tool: Tool, plan: &CoreAssurancePlan) -> Result<Command, CoreMiriError> {
    let mut command = Command::new("docker");
    match tool {
        Tool::Docker => {
            command.args(tool.arguments());
        }
        Tool::Rustc | Tool::Cargo | Tool::Miri => {
            let step = plan.step(Phase::MiriSetup);
            let mut arguments = step.arguments;
            let prefix = arguments.len().checked_sub(4).ok_or(CoreMiriError::Plan)?;
            let expected = [
                "cargo".to_owned(),
                format!("+{NIGHTLY}"),
                "miri".to_owned(),
                "setup".to_owned(),
            ];
            if arguments.get(prefix..) != Some(expected.as_slice()) {
                return Err(CoreMiriError::Plan);
            }
            arguments.truncate(prefix);
            let timeout = arguments.last_mut().ok_or(CoreMiriError::Plan)?;
            if timeout != "880s" {
                return Err(CoreMiriError::Plan);
            }
            "10s".clone_into(timeout);
            for argument in &mut arguments {
                match argument.as_str() {
                    "--network=bridge" => "--network=none".clone_into(argument),
                    "--env=CARGO_NET_OFFLINE=false" => {
                        "--env=CARGO_NET_OFFLINE=true".clone_into(argument)
                    }
                    _ => {}
                }
            }
            command.args(arguments).args(tool.arguments());
        }
    }
    Ok(command)
}

pub(super) fn capture(
    state: &Path,
    plan: &CoreAssurancePlan,
    perform: &mut impl FnMut(&mut Command, Duration, CommandMode) -> Result<(), ProcessError>,
) -> Result<(), CoreMiriError> {
    fs::create_dir(state.join("tools"))?;
    for tool in Tool::ALL {
        let mut command = command(tool, plan)?;
        for stream in ["stdout", "stderr"] {
            let file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(state.join(format!("tools/{}.{stream}.txt", tool.name())))?;
            match stream {
                "stdout" => {
                    command.stdout(file);
                }
                "stderr" => {
                    command.stderr(file);
                }
                _ => return Err(CoreMiriError::Plan),
            }
        }
        let result = perform(&mut command, Duration::from_secs(15), CommandMode::Work);
        match tool {
            Tool::Docker => result.map_err(CoreMiriError::Process)?,
            Tool::Rustc | Tool::Cargo | Tool::Miri => {
                container_result(result, plan, Phase::MiriSetup, perform)?;
            }
        }
    }
    check(state)
}
