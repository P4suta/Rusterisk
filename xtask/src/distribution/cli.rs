use crate::process::{cargo_path, CommandStream, CommandSupervisor, SupervisedCommand};
use std::{ffi::OsStr, io, path::Path, time::Duration};

pub fn verify_cli(root: &Path) -> io::Result<()> {
    let mut supervisor = CommandSupervisor::new().map_err(io::Error::other)?;
    let work = tempfile::Builder::new()
        .prefix("rusterisk-cli-")
        .tempdir()?;
    let result = install_and_run(&mut supervisor, root, work.path());
    let result = cleanup(result, supervisor.finish().map_err(io::Error::other));
    cleanup(result, work.close())
}

fn cleanup(result: io::Result<()>, cleanup: io::Result<()>) -> io::Result<()> {
    match (result, cleanup) {
        (Ok(()), cleanup) => cleanup,
        (Err(error), Ok(())) => Err(error),
        (Err(error), Err(cleanup)) => Err(io::Error::other(format!(
            "{error}; CLI verification cleanup failed: {cleanup}"
        ))),
    }
}

fn run(
    supervisor: &mut CommandSupervisor,
    program: impl AsRef<OsStr>,
    directory: &Path,
    arguments: &[&str],
    paths: &[(&str, &Path)],
    budget: Duration,
) -> io::Result<()> {
    let mut command = SupervisedCommand::new(program).map_err(io::Error::other)?;
    command.current_dir(directory).map_err(io::Error::other)?;
    for argument in arguments {
        command.arg(argument).map_err(io::Error::other)?;
    }
    for (option, path) in paths {
        command
            .arg(option)
            .and_then(|command| command.arg(path))
            .map_err(io::Error::other)?;
    }
    command
        .stdout(CommandStream::Inherit)
        .stderr(CommandStream::Inherit);
    supervisor
        .run_owned(command, budget)
        .map_err(io::Error::other)
}

fn install_and_run(supervisor: &mut CommandSupervisor, root: &Path, work: &Path) -> io::Result<()> {
    let work = cargo_path(work)?;
    let target = work.join("target");
    let installed = work.join("installed");
    let version = env!("CARGO_PKG_VERSION");
    let package = target
        .join("package")
        .join(format!("rusterisk-cli-{version}"));
    let cargo_budget = Duration::from_secs(900);
    run(
        supervisor,
        "cargo",
        root,
        &[
            "package",
            "--locked",
            "--allow-dirty",
            "-p",
            "rusterisk-cli",
            "--jobs",
            "2",
        ],
        &[("--target-dir", &target)],
        cargo_budget,
    )?;
    for license in ["LICENSE-MIT", "LICENSE-APACHE"] {
        if std::fs::read(package.join(license))? != std::fs::read(root.join(license))? {
            return Err(io::Error::other(
                "CLI package license differs from the workspace",
            ));
        }
    }
    run(
        supervisor,
        "cargo",
        &package,
        &["test", "--locked", "--jobs", "2"],
        &[("--target-dir", &target)],
        cargo_budget,
    )?;
    run(
        supervisor,
        "cargo",
        work,
        &["install", "--offline", "--locked", "--debug", "--jobs", "2"],
        &[
            ("--root", &installed),
            ("--target-dir", &target),
            ("--path", &package),
        ],
        cargo_budget,
    )?;
    let binary = installed
        .join("bin")
        .join(format!("rusterisk-cli{}", std::env::consts::EXE_SUFFIX));
    run(
        supervisor,
        binary,
        work,
        &["--version"],
        &[],
        Duration::from_secs(10),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn distribution_rejects_a_failing_packaged_test() -> io::Result<()> {
        let fixture = tempfile::tempdir()?;
        let root = fixture.path();
        fs::create_dir(root.join("src"))?;
        fs::create_dir(root.join("tests"))?;
        fs::write(root.join("LICENSE-MIT"), "fixture")?;
        fs::write(root.join("LICENSE-APACHE"), "fixture")?;
        fs::write(root.join("src/main.rs"), "fn main() {}")?;
        let version = env!("CARGO_PKG_VERSION");
        fs::write(
            root.join("Cargo.toml"),
            format!("[package]\nname='rusterisk-cli'\nversion='{version}'\nedition='2021'\nlicense='MIT OR Apache-2.0'\n"),
        )?;
        fs::write(
            root.join("Cargo.lock"),
            format!("version = 4\n\n[[package]]\nname = 'rusterisk-cli'\nversion = '{version}'\n"),
        )?;
        let test = root.join("tests/packaged.rs");
        fs::write(&test, "#[test] fn contents() { assert_eq!(1, 1); }")?;
        verify_cli(root)?;
        fs::write(test, "#[test] fn contents() { assert_eq!(1, 2); }")?;
        assert!(verify_cli(root).is_err());
        Ok(())
    }
}
