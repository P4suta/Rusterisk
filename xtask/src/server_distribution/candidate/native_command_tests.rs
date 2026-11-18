use super::CapturedServer;
use crate::{process::CommandSupervisor, server_distribution::ServerTarget};
use std::{io, time::Duration};

mod fixture {
    use crate as xtask;
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/sdk_candidate_build/fixture.rs"
    ));
}

fn require(valid: bool, message: &str) -> io::Result<()> {
    if valid {
        Ok(())
    } else {
        Err(io::Error::other(message))
    }
}

#[test]
fn native_windows_fetch_preserves_actual_cargo_failure_diagnostics(
) -> Result<(), crate::distribution::SdkCandidateBuildError> {
    fixture::using_project("native-windows-fetch", |root, parent| {
        fixture::write(
            root,
            "docs/server-distribution.md",
            b"Startup instructions.\n",
        )?;
        fixture::write(root, ".cargo/config.toml", b"[build\n")?;
        let captured = CapturedServer::read(root, ServerTarget::WindowsX86_64)?;
        let destination = parent.join("candidate");
        let mut supervisor = CommandSupervisor::new().map_err(io::Error::other)?;
        let mut calls = 0;
        let operation = captured.build_with(&destination, |command, budget| {
            calls += 1;
            require(
                budget == Duration::from_secs(180),
                "Native server fetch changed its phase deadline",
            )?;
            supervisor.run_owned(command, budget)
        });
        let cleanup = supervisor.finish().map_err(io::Error::other);
        let validation = (|| {
            require(calls == 1, "Failed native fetch reached another phase")?;
            let error = operation
                .err()
                .ok_or_else(|| io::Error::other("Malformed native Cargo configuration passed"))?;
            require(
                error.to_string().contains("process failed:"),
                &format!("Native server fetch did not reach Cargo: {error}"),
            )?;
            let diagnostic = std::fs::read_to_string(destination.join("fetch.txt"))?;
            require(
                diagnostic.contains("error:")
                    && diagnostic.contains(".cargo")
                    && diagnostic.contains("config.toml"),
                &format!("Native server fetch lost Cargo configuration diagnostics: {diagnostic}"),
            )?;
            require(
                !destination.join("build.txt").exists() && !destination.join("SHA256SUMS").exists(),
                "Failed native fetch published a build or archive",
            )?;
            captured.check(root)?;
            captured.check(&destination.join("source"))
        })();
        super::combine(
            validation,
            cleanup,
            "native fetch supervisor cleanup failed",
        )?;
        Ok(())
    })
}
