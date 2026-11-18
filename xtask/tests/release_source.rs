use std::{
    io,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};
use xtask::{
    core_proofs::CapturedCoreProofs,
    release_check::{CapturedReleaseSource, InputError, SourceInputs},
};

fn using_source(
    label: &str,
    test: impl FnOnce(&CapturedReleaseSource, &Path) -> io::Result<()>,
) -> io::Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or_else(|| io::Error::other("workspace missing"))?;
    let captured = CapturedReleaseSource::read(root).map_err(io::Error::other)?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    let parent = std::env::temp_dir().join(format!(
        "rusterisk-release-source-{}-{stamp}-{label}",
        std::process::id()
    ));
    std::fs::create_dir(&parent)?;
    let source = parent.join("source");
    let operation = captured
        .write(&source, &parent.join("state"))
        .map_err(io::Error::other)
        .and_then(|()| test(&captured, &source));
    let cleanup = std::fs::remove_dir_all(parent);
    match (operation, cleanup) {
        (result, Ok(())) => result,
        (Ok(()), Err(error)) => Err(error),
        (Err(operation), Err(cleanup)) => Err(io::Error::other(format!(
            "{operation}; release source cleanup failed: {cleanup}"
        ))),
    }
}

#[test]
fn captured_sources_keep_required_empty_directories_through_copy_and_check() -> io::Result<()> {
    using_source("empty-directories", |_captured, source| {
        let directories = [
            "api",
            "containers",
            ".github/workflows",
            "tests/interop/sipp",
            "docs/adr",
        ];
        for directory in directories {
            let path = source.join(directory);
            std::fs::remove_dir_all(&path)?;
            std::fs::create_dir_all(path)?;
        }
        let empty = CapturedReleaseSource::read(source).map_err(io::Error::other)?;
        let copy = source.with_file_name("empty-source-copy");
        empty
            .write(&copy, &source.with_file_name("empty-source-state"))
            .map_err(io::Error::other)?;
        for directory in directories {
            assert!(copy.join(directory).is_dir(), "{directory}");
        }
        empty.check(&copy).map_err(io::Error::other)
    })
}

#[test]
fn one_release_source_contains_each_runner_projection_and_its_governing_inputs() -> io::Result<()> {
    using_source("projections", |captured, source| {
        let browser = xtask::browser::SourceSnapshot::read(source).map_err(io::Error::other)?;
        let distribution =
            xtask::distribution::SourceSnapshot::read(source).map_err(io::Error::other)?;
        let proofs = CapturedCoreProofs::read(source).map_err(io::Error::other)?;
        let cohort = captured.inputs().map_err(io::Error::other)?;
        for projection in [
            SourceInputs::new(browser.files()).map_err(io::Error::other)?,
            SourceInputs::new(distribution.files()).map_err(io::Error::other)?,
            SourceInputs::new(proofs.inputs()).map_err(io::Error::other)?,
        ] {
            assert_eq!(
                cohort.bind(&projection).map_err(io::Error::other)?,
                projection.key()
            );
        }
        for path in [
            "crates/core/proofs/Cargo.lock",
            "crates/core/assurance/Cargo.lock",
            "rust-toolchain.toml",
            "clippy.toml",
            "deny.toml",
            "justfile",
            "api/policy-exceptions.toml",
            "api/gitleaks.toml",
            ".github/workflows/check.yml",
            "containers/core-proofs/Dockerfile",
            "containers/input-verification/Dockerfile",
            "containers/browser-verification/Dockerfile",
            "containers/asterisk-reference/Dockerfile",
            "tests/interop/node-ari-client/package-lock.json",
            "tests/interop/node-ari-client/contracts.ts",
            "tests/interop/node-ari-client/scenario.ts",
        ] {
            assert!(
                captured
                    .files()
                    .any(|(actual, bytes)| actual == Path::new(path) && !bytes.is_empty()),
                "missing release input: {path}"
            );
        }
        captured.check(source).map_err(io::Error::other)?;
        Ok(())
    })
}

#[test]
fn captured_release_inputs_supply_every_registered_policy_decision() -> io::Result<()> {
    using_source("policy-decisions", |_captured, source| {
        let exceptions: xtask::Exceptions = toml::from_str(&std::fs::read_to_string(
            source.join("api/policy-exceptions.toml"),
        )?)
        .map_err(io::Error::other)?;
        if exceptions.exceptions.is_empty() {
            return Err(io::Error::other("policy decision fixture is empty"));
        }
        for exception in exceptions.exceptions {
            let path = source.join(&exception.adr);
            if !path.is_file() {
                return Err(io::Error::other(format!(
                    "captured policy decision missing: {}",
                    exception.adr
                )));
            }
        }
        Ok(())
    })
}

#[test]
fn captured_release_inputs_supply_every_server_target_projection() -> io::Result<()> {
    using_source("server-projections", |captured, source| {
        let cohort = captured.inputs().map_err(io::Error::other)?;
        for target in xtask::server_distribution::ServerTarget::ALL {
            let server = xtask::server_distribution::CapturedServer::read(source, target)
                .map_err(io::Error::other)?;
            let projection = SourceInputs::new(server.files()).map_err(io::Error::other)?;
            cohort.bind(&projection).map_err(io::Error::other)?;
        }
        Ok(())
    })
}

#[test]
fn captured_release_inputs_supply_the_actual_reference_configuration() -> io::Result<()> {
    using_source("reference-configuration", |_captured, source| {
        xtask::reference::copy_configuration(
            &source.join("tests/interop/asterisk"),
            &source.with_file_name("reference-configuration"),
        )
    })
}

#[test]
fn captured_release_inputs_supply_and_check_the_exact_sipp_scenario() -> io::Result<()> {
    using_source("sipp-scenario", |captured, source| {
        let path = Path::new("tests/interop/sipp/registration.xml");
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .ok_or_else(|| io::Error::other("workspace missing"))?;
        let original = std::fs::read(root.join(path))?;
        assert_eq!(std::fs::read(source.join(path))?, original);
        let projection = SourceInputs::new(std::iter::once((path, original.as_slice())))
            .map_err(io::Error::other)?;
        let cohort = captured.inputs().map_err(io::Error::other)?;
        cohort.bind(&projection).map_err(io::Error::other)?;
        std::fs::write(source.join(path), b"<scenario name=\"changed\" />\n")?;
        assert!(captured.check(source).is_err());
        Ok(())
    })
}

#[test]
fn changed_or_added_verification_inputs_cannot_bind_to_an_earlier_capture() -> io::Result<()> {
    using_source("changes", |captured, source| {
        let cohort = captured.inputs().map_err(io::Error::other)?;
        let recipe = source.join("containers/core-proofs/Dockerfile");
        let original = std::fs::read(&recipe)?;
        std::fs::write(&recipe, b"FROM changed.invalid\n")?;
        let changed = CapturedReleaseSource::read(source).map_err(io::Error::other)?;
        assert!(matches!(
            cohort.bind(&changed.inputs().map_err(io::Error::other)?),
            Err(InputError::Changed)
        ));
        assert!(captured.check(source).is_err());
        std::fs::write(&recipe, original)?;
        std::fs::write(
            source.join("tests/interop/node-ari-client/additional_contract.ts"),
            b"export {};\n",
        )?;
        let added = CapturedReleaseSource::read(source).map_err(io::Error::other)?;
        assert!(matches!(
            cohort.bind(&added.inputs().map_err(io::Error::other)?),
            Err(InputError::Missing)
        ));
        assert!(captured.check(source).is_err());
        Ok(())
    })
}

#[test]
fn absent_oversize_and_redirected_proof_locks_are_refused() -> io::Result<()> {
    using_source("locks", |_captured, source| {
        let lock = source.join("crates/core/proofs/Cargo.lock");
        let original = std::fs::read(&lock)?;
        std::fs::remove_file(&lock)?;
        assert!(CapturedReleaseSource::read(source).is_err());
        std::fs::write(&lock, &original)?;
        std::fs::OpenOptions::new()
            .write(true)
            .open(&lock)?
            .set_len(1024 * 1024 + 1)?;
        assert!(CapturedReleaseSource::read(source).is_err());
        std::fs::write(&lock, original)?;
        #[cfg(unix)]
        {
            std::fs::remove_file(&lock)?;
            std::os::unix::fs::symlink(source.join("Cargo.lock"), &lock)?;
            assert!(CapturedReleaseSource::read(source).is_err());
        }
        Ok(())
    })
}

#[test]
fn verification_inputs_share_the_project_file_and_byte_budgets() -> io::Result<()> {
    using_source("file-budget", |captured, source| {
        let remaining = SourceInputs::MAX_FILES - captured.files().count();
        for index in 0..remaining {
            std::fs::write(source.join(format!("api/padding-{index}.toml")), b" ")?;
        }
        let full = CapturedReleaseSource::read(source).map_err(io::Error::other)?;
        assert_eq!(full.files().count(), SourceInputs::MAX_FILES);
        std::fs::write(source.join("api/excess.toml"), b" ")?;
        assert!(CapturedReleaseSource::read(source).is_err());
        Ok(())
    })?;
    using_source("byte-budget", |captured, source| {
        let total: usize = captured.files().map(|(_, bytes)| bytes.len()).sum();
        let mut remaining = SourceInputs::MAX_TOTAL_BYTES - total;
        let mut index = 0;
        while remaining != 0 {
            let length = remaining.min(1024 * 1024);
            std::fs::write(
                source.join(format!("api/padding-{index}.toml")),
                vec![b' '; length],
            )?;
            index += 1;
            remaining -= length;
        }
        let full = CapturedReleaseSource::read(source).map_err(io::Error::other)?;
        assert_eq!(
            full.files().map(|(_, bytes)| bytes.len()).sum::<usize>(),
            SourceInputs::MAX_TOTAL_BYTES
        );
        std::fs::write(source.join("api/excess.toml"), b" ")?;
        assert!(CapturedReleaseSource::read(source).is_err());
        Ok(())
    })
}

#[cfg(unix)]
#[test]
fn linked_verification_parent_directories_cannot_redirect_the_capture() -> io::Result<()> {
    using_source("linked-parent", |_captured, source| {
        let github = source.join(".github");
        let outside = source.with_file_name("outside-workflows");
        std::fs::rename(&github, &outside)?;
        std::os::unix::fs::symlink(outside, github)?;
        assert!(CapturedReleaseSource::read(source).is_err());
        Ok(())
    })
}
