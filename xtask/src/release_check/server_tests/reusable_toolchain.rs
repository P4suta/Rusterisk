use super::super::{verify_server_recorded, Retention, ServerRequest};
use super::{native_target, require, using};
use crate::{
    process::CommandSupervisor, release_check::CapturedReleaseSource,
    server_distribution::ServerExecutionMode,
};
use std::{ffi::OsStr, fs, io};

#[test]
fn reusable_sdk_commands_use_the_captured_toolchain_from_their_isolated_directory() -> io::Result<()>
{
    using(native_target()?, |_captured, paths| {
        let toolchain = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../rust-toolchain.toml"
        ));
        fs::write(paths.sdk.join("source/rust-toolchain.toml"), toolchain)?;
        let captured =
            CapturedReleaseSource::read(&paths.sdk.join("source")).map_err(io::Error::other)?;
        let declaration: toml::Value = toml::from_str(toolchain).map_err(io::Error::other)?;
        let expected = declaration
            .get("toolchain")
            .and_then(|table| table.get("channel"))
            .and_then(toml::Value::as_str)
            .ok_or_else(|| io::Error::other("Fixture toolchain channel missing"))?;
        fs::create_dir(&paths.artifacts)?;
        let native = paths.artifacts.join("native");
        let supervisor = CommandSupervisor::new().map_err(io::Error::other)?;
        let mut observed = None;
        let result = verify_server_recorded(
            &captured,
            ServerRequest {
                candidate: &paths.server,
                target: paths.target,
                mode: ServerExecutionMode::Native,
                sdk_candidate: &paths.sdk,
                execution: &paths.execution,
                consumer_execution: &paths.consumer_execution,
                artifacts: &native,
            },
            supervisor,
            |command, _budget| {
                require(
                    command.get_program() == "cargo",
                    "Toolchain fixture did not reach SDK preparation",
                )?;
                require(
                    command
                        .get_current_dir()
                        .is_some_and(|path| path.starts_with(&paths.consumer_execution)),
                    "SDK command was not prepared in its isolated directory",
                )?;
                observed = Some(
                    command
                        .get_envs()
                        .find(|(name, _)| *name == "RUSTUP_TOOLCHAIN")
                        .and_then(|(_, value)| value.map(OsStr::to_owned)),
                );
                Err(io::Error::other("stop after observing the SDK toolchain"))
            },
            Retention::Reusable(&paths.artifacts),
        );
        let error = result.err().ok_or_else(|| {
            io::Error::other("Toolchain fixture executed its synthetic candidate")
        })?;
        require(
            error
                .to_string()
                .contains("stop after observing the SDK toolchain"),
            "Toolchain fixture stopped before its SDK boundary",
        )?;
        require(
            observed.as_ref().is_some_and(|value| value.as_deref() == Some(OsStr::new(expected))),
            "SDK command did not select the captured toolchain independently of its working directory",
        )?;
        require(
            !paths.execution.exists() && !paths.consumer_execution.exists(),
            "Failed toolchain observation retained owned execution state",
        )
    })
}
