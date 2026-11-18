use super::{FuzzError, Snapshot};
use crate::process::ProcessError;
use std::{
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

fn fixture(
    name: &str,
    operation: impl FnOnce(&Path, &Path) -> Result<(), FuzzError>,
) -> Result<(), FuzzError> {
    let identity = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| FuzzError::Plan)?
        .as_nanos();
    let directory = std::env::temp_dir().join(format!(
        "rusterisk-fuzz-{name}-{}-{identity}",
        std::process::id()
    ));
    std::fs::create_dir(&directory)?;
    let root = directory.join("root");
    let result = (|| {
        super::write_new(
            &root.join("Cargo.toml"),
            b"[workspace]\nmembers=['xtask']\n",
        )?;
        for file in ["Cargo.lock", "xtask/fuzz/Cargo.lock"] {
            super::write_new(
                &root.join(file),
                b"[[package]]\nname='fixture'\nversion='0.1.0'\n",
            )?;
        }
        super::write_new(
            &root.join("xtask/fuzz/Cargo.toml"),
            include_bytes!("../../../fuzz/Cargo.toml"),
        )?;
        for file in [
            "xtask/Cargo.toml",
            "xtask/src/lib.rs",
            "xtask/tests/contract.rs",
            "xtask/fuzz/src/lib.rs",
            "xtask/fuzz/tests/contract.rs",
            "xtask/fuzz/fuzz_targets/settings.rs",
        ] {
            super::write_new(&root.join(file), b"fixture")?;
        }
        operation(&root, &directory)
    })();
    match (result, std::fs::remove_dir_all(directory)) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(error)) => Err(FuzzError::Io(error)),
        (Err(error), Err(cleanup)) => Err(FuzzError::Cleanup {
            operation: error.to_string(),
            cleanup: ProcessError::Io(cleanup),
        }),
    }
}

const PATCHED: &[u8] =
    b"[workspace]\nmembers=['xtask']\n[patch.crates-io]\nstr0m={path='vendor/str0m'}\n";

#[test]
fn project_and_vendor_file_budgets_accept_each_complete_bounded_source_tree(
) -> Result<(), FuzzError> {
    fixture("independent-file-budgets", |root, _| {
        use crate::release_check::{InputError, SourceInputs};
        assert_eq!(
            super::MAX_PROJECT_FILES + super::MAX_VENDOR_FILES,
            SourceInputs::MAX_FILES
        );
        assert_eq!(super::MAX_SNAPSHOT_BYTES, SourceInputs::MAX_TOTAL_BYTES);
        assert_eq!(super::MAX_VENDOR_FILES, 512);
        vendor(root)?;
        let baseline = Snapshot::read(root)?;
        let vendor_files = baseline
            .files
            .iter()
            .filter(|file| file.path.starts_with("vendor/str0m"))
            .count();
        let project_files = baseline.files.len() - vendor_files;
        for number in project_files..super::MAX_PROJECT_FILES {
            super::write_new(
                &root.join(format!("xtask/src/count-{number}.rs")),
                b"fixture",
            )?;
        }
        for number in vendor_files..super::MAX_VENDOR_FILES {
            super::write_new(
                &root.join(format!("vendor/str0m/count-{number}.rs")),
                b"fixture",
            )?;
        }
        let snapshot = Snapshot::read(root)?;
        assert_eq!(snapshot.files.len(), SourceInputs::MAX_FILES);
        let inputs = || {
            snapshot
                .files
                .iter()
                .map(|file| (file.path.as_path(), file.bytes.as_slice()))
        };
        assert!(SourceInputs::new(inputs()).is_ok());
        assert!(matches!(
            SourceInputs::new(inputs().chain(std::iter::once((
                Path::new("excess.rs"),
                b"fixture".as_slice()
            )))),
            Err(InputError::Limit)
        ));
        let project_excess = root.join("xtask/src/count-excess.rs");
        super::write_new(&project_excess, b"fixture")?;
        assert!(matches!(
            Snapshot::read(root),
            Err(FuzzError::FileCount {
                scope: super::FuzzFileScope::Project,
                maximum
            }) if maximum == super::MAX_PROJECT_FILES
        ));
        std::fs::remove_file(project_excess)?;
        super::write_new(&root.join("vendor/str0m/count-excess.rs"), b"fixture")?;
        assert!(matches!(
            Snapshot::read(root),
            Err(FuzzError::FileCount {
                scope: super::FuzzFileScope::Vendor,
                maximum
            }) if maximum == super::MAX_VENDOR_FILES
        ));
        Ok(())
    })
}

#[test]
fn vendored_dependency_is_captured_only_for_the_exact_workspace_patch() -> Result<(), FuzzError> {
    fixture("vendor", |root, directory| {
        for file in [
            "vendor/str0m/src/lib.rs",
            "vendor/str0m/Cargo.toml",
            "vendor/str0m-source.toml",
            "vendor/str0m.patch",
            "vendor/archives/str0m-0.25.0.crate",
        ] {
            super::write_new(&root.join(file), b"vendor fixture")?;
        }
        assert!(Snapshot::read(root)?
            .files
            .iter()
            .all(|file| !file.path.starts_with("vendor")));
        std::fs::write(root.join("Cargo.toml"), PATCHED)?;
        let snapshot = Snapshot::read(root)?;
        for file in [
            "vendor/str0m/src/lib.rs",
            "vendor/str0m/Cargo.toml",
            "vendor/str0m-source.toml",
            "vendor/str0m.patch",
            "vendor/archives/str0m-0.25.0.crate",
        ] {
            assert!(snapshot
                .files
                .iter()
                .any(|input| input.path == Path::new(file)));
        }
        let source = directory.join("source");
        snapshot.write(&source, &directory.join("state"))?;
        snapshot.verify_source(&source)?;
        assert_eq!(
            std::fs::read(source.join("vendor/str0m/src/lib.rs"))?,
            b"vendor fixture"
        );
        for file in [
            "vendor/str0m/src/lib.rs",
            "vendor/str0m-source.toml",
            "vendor/str0m.patch",
            "vendor/archives/str0m-0.25.0.crate",
        ] {
            std::fs::write(source.join(file), b"changed")?;
            assert!(matches!(
                snapshot.verify_source(&source),
                Err(FuzzError::SourceChanged)
            ));
            std::fs::write(source.join(file), b"vendor fixture")?;
        }
        super::write_new(&source.join("vendor/str0m/src/hidden.rs"), b"added")?;
        assert!(matches!(
            snapshot.verify_source(&source),
            Err(FuzzError::SourceChanged)
        ));
        std::fs::remove_file(source.join("vendor/str0m/src/hidden.rs"))?;
        std::fs::remove_file(source.join("vendor/str0m.patch"))?;
        assert!(snapshot.verify_source(&source).is_err());
        for path in ["other/str0m", "../str0m", "/tmp/str0m"] {
            std::fs::write(
                root.join("Cargo.toml"),
                format!(
                    "[workspace]\nmembers=['xtask']\n[patch.crates-io]\nstr0m={{path='{path}'}}\n"
                ),
            )?;
            assert!(Snapshot::read(root).is_err());
        }
        Ok(())
    })
}

fn vendor(root: &Path) -> Result<(), FuzzError> {
    std::fs::write(root.join("Cargo.toml"), PATCHED)?;
    for file in [
        "vendor/str0m/Cargo.toml",
        "vendor/str0m-source.toml",
        "vendor/str0m.patch",
        "vendor/archives/str0m-0.25.0.crate",
    ] {
        super::write_new(&root.join(file), b"fixture")?;
    }
    Ok(())
}

fn sparse(root: &Path, file: &str, size: u64) -> Result<(), FuzzError> {
    super::write_new(&root.join(file), b"")?;
    std::fs::OpenOptions::new()
        .write(true)
        .open(root.join(file))?
        .set_len(size)?;
    Ok(())
}

#[test]
fn vendor_bounds_preserve_project_file_limits_and_bound_the_tree_and_complete_snapshot(
) -> Result<(), FuzzError> {
    fixture("vendor-bounds", |root, _| {
        vendor(root)?;
        sparse(root, "vendor/str0m/large", 1024 * 1024 + 1)?;
        assert!(Snapshot::read(root).is_ok());
        let large = std::fs::OpenOptions::new()
            .write(true)
            .open(root.join("vendor/str0m/large"))?;
        large.set_len(8 * 1024 * 1024)?;
        assert!(Snapshot::read(root).is_ok());
        large.set_len(8 * 1024 * 1024 + 1)?;
        assert!(Snapshot::read(root).is_err());
        large.set_len(6 * 1024 * 1024)?;
        sparse(root, "vendor/str0m/second", 6 * 1024 * 1024 - 7)?;
        assert!(Snapshot::read(root).is_ok());
        super::write_new(&root.join("vendor/str0m/excess"), b"x")?;
        assert!(Snapshot::read(root).is_err());
        std::fs::remove_file(root.join("vendor/str0m/excess"))?;
        sparse(root, "xtask/src/oversized.rs", 1024 * 1024 + 1)?;
        assert!(Snapshot::read(root).is_err());
        std::fs::remove_file(root.join("xtask/src/oversized.rs"))?;
        for number in 0..27 {
            sparse(root, &format!("xtask/src/part{number}.rs"), 1024 * 1024)?;
        }
        assert!(Snapshot::read(root).is_ok());
        sparse(root, "xtask/src/excess.rs", 1024 * 1024)?;
        assert!(Snapshot::read(root).is_err());
        Ok(())
    })
}

#[cfg(unix)]
#[test]
fn vendor_symlinks_are_refused_at_each_tree_and_record_boundary() -> Result<(), FuzzError> {
    for (name, path) in [
        ("vendor-parent", "vendor"),
        ("vendor-tree", "vendor/str0m"),
        ("vendor-file", "vendor/str0m/Cargo.toml"),
        ("vendor-record", "vendor/str0m-source.toml"),
        ("vendor-patch", "vendor/str0m.patch"),
        ("archive-parent", "vendor/archives"),
        ("archive-file", "vendor/archives/str0m-0.25.0.crate"),
    ] {
        fixture(name, |root, directory| {
            vendor(root)?;
            let outside = directory.join("outside");
            std::fs::rename(root.join(path), &outside)?;
            std::os::unix::fs::symlink(outside, root.join(path))?;
            assert!(Snapshot::read(root).is_err());
            Ok(())
        })?;
    }
    Ok(())
}

#[test]
fn published_archive_has_a_separate_file_limit_and_does_not_consume_the_vendor_tree_budget(
) -> Result<(), FuzzError> {
    fixture("archive-bounds", |root, directory| {
        vendor(root)?;
        let name = "vendor/archives/str0m-0.25.0.crate";
        std::fs::OpenOptions::new()
            .write(true)
            .open(root.join(name))?
            .set_len(8 * 1024 * 1024)?;
        sparse(root, "vendor/str0m/left", 6 * 1024 * 1024)?;
        sparse(root, "vendor/str0m/right", 6 * 1024 * 1024 - 7)?;
        let snapshot = Snapshot::read(root)?;
        let source = directory.join("source");
        snapshot.write(&source, &directory.join("state"))?;
        assert_eq!(std::fs::metadata(source.join(name))?.len(), 8 * 1024 * 1024);
        std::fs::OpenOptions::new()
            .write(true)
            .open(root.join(name))?
            .set_len(8 * 1024 * 1024 + 1)?;
        assert!(Snapshot::read(root).is_err());
        std::fs::remove_file(root.join(name))?;
        assert!(Snapshot::read(root).is_err());
        Ok(())
    })
}

#[test]
fn captured_sources_remain_bound_while_independent_checkout_edits_continue() -> Result<(), FuzzError>
{
    let project = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or(FuzzError::Manifest)?;
    let snapshot = Snapshot::read(project)?;
    let identity = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| FuzzError::Plan)?
        .as_nanos();
    let directory = std::env::temp_dir().join(format!(
        "rusterisk-fuzz-snapshot-{}-{identity}",
        std::process::id()
    ));
    std::fs::create_dir(&directory)?;
    let result = (|| {
        let checkout = directory.join("checkout");
        let source = directory.join("source");
        snapshot.write(&checkout, &directory.join("checkout-state"))?;
        snapshot.write(&source, &directory.join("state"))?;
        snapshot.verify_source(&source)?;
        std::fs::write(
            checkout.join("crates/config/src/lib.rs"),
            b"changed checkout",
        )?;
        snapshot.verify_source(&source)?;
        std::fs::write(
            source.join("crates/config/src/lib.rs"),
            b"changed captured source",
        )?;
        assert!(matches!(
            snapshot.verify_source(&source),
            Err(FuzzError::SourceChanged)
        ));
        Ok(())
    })();
    match (result, std::fs::remove_dir_all(directory)) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(error)) => Err(FuzzError::Io(error)),
        (Err(error), Err(cleanup)) => Err(FuzzError::Cleanup {
            operation: error.to_string(),
            cleanup: ProcessError::Io(cleanup),
        }),
    }
}

#[test]
fn harness_tree_refuses_added_configuration_source_or_missing_inputs() -> Result<(), FuzzError> {
    for path in [
        ".cargo/config.toml",
        ".cargo/config",
        "src/hidden.rs",
        "tests/hidden.rs",
        "fuzz_targets/hidden.rs",
        "build.rs",
    ] {
        fixture("added-harness-input", |root, directory| {
            let snapshot = Snapshot::read(root)?;
            let state = directory.join("state");
            snapshot.write(&directory.join("source"), &state)?;
            snapshot.check_harness(&state)?;
            super::write_new(&state.join("fuzz").join(path), b"added")?;
            if snapshot.check_harness(&state).is_ok() {
                return Err(FuzzError::Io(std::io::Error::other(format!(
                    "unexpected harness input accepted: {path}"
                ))));
            }
            Ok(())
        })?;
    }
    for path in [
        "Cargo.toml",
        "Cargo.lock",
        "src/lib.rs",
        "tests/contract.rs",
        "fuzz_targets/settings.rs",
    ] {
        fixture("missing-harness-input", |root, directory| {
            let snapshot = Snapshot::read(root)?;
            let state = directory.join("state");
            snapshot.write(&directory.join("source"), &state)?;
            std::fs::remove_file(state.join("fuzz").join(path))?;
            if snapshot.check_harness(&state).is_ok() {
                return Err(FuzzError::Io(std::io::Error::other(format!(
                    "missing harness input accepted: {path}"
                ))));
            }
            Ok(())
        })?;
    }
    Ok(())
}

#[test]
fn captured_source_refuses_added_root_build_configuration() -> Result<(), FuzzError> {
    for path in [".cargo/config.toml", ".cargo/config", "build.rs"] {
        fixture("added-source-input", |root, directory| {
            let snapshot = Snapshot::read(root)?;
            let source = directory.join("source");
            snapshot.write(&source, &directory.join("state"))?;
            super::write_new(&source.join(path), b"added")?;
            if snapshot.verify_source(&source).is_ok() {
                return Err(FuzzError::Io(std::io::Error::other(format!(
                    "unexpected captured source input accepted: {path}"
                ))));
            }
            Ok(())
        })?;
    }
    Ok(())
}

#[test]
fn harness_checks_allow_owned_outputs_outside_the_exact_harness_tree() -> Result<(), FuzzError> {
    fixture("state-output-boundary", |root, directory| {
        let snapshot = Snapshot::read(root)?;
        let source = directory.join("source");
        let state = directory.join("state");
        snapshot.write(&source, &state)?;
        for path in [
            "target/native-binary",
            "corpus/settings/discovered-input",
            "artifacts/settings/crash-input",
        ] {
            super::write_new(&state.join(path), b"generated")?;
        }
        snapshot.check_harness(&state)?;
        snapshot.verify_source(&source)
    })
}

#[cfg(unix)]
#[test]
fn native_readers_refuse_linked_files_and_every_owned_ancestor() -> Result<(), FuzzError> {
    for path in ["state", "state/deep", "state/deep/native.txt"] {
        fixture("native-link-boundary", |_, directory| {
            super::write_new(&directory.join("state/deep/native.txt"), b"native")?;
            let outside = directory.join("outside");
            std::fs::rename(directory.join(path), &outside)?;
            std::os::unix::fs::symlink(outside, directory.join(path))?;
            assert!(super::read_under(
                directory,
                Path::new("state/deep/native.txt"),
                4 * 1024 * 1024
            )
            .is_err());
            if path != "state/deep/native.txt" {
                assert!(super::read_directory(directory, Path::new("state/deep")).is_err());
            }
            Ok(())
        })?;
    }
    fixture("native-size-boundary", |_, directory| {
        let relative = Path::new("state/native.txt");
        sparse(directory, "state/native.txt", 4 * 1024 * 1024)?;
        assert_eq!(
            super::read_under(directory, relative, 4 * 1024 * 1024)?.len(),
            4 * 1024 * 1024
        );
        std::fs::OpenOptions::new()
            .write(true)
            .open(directory.join(relative))?
            .set_len(4 * 1024 * 1024 + 1)?;
        assert!(super::read_under(directory, relative, 4 * 1024 * 1024).is_err());
        Ok(())
    })
}

#[cfg(unix)]
#[test]
fn harness_tree_refuses_linked_roots_directories_and_files() -> Result<(), FuzzError> {
    for path in [
        "state",
        "state/fuzz",
        "state/fuzz/src",
        "state/fuzz/src/lib.rs",
    ] {
        fixture("harness-link-boundary", |root, directory| {
            let snapshot = Snapshot::read(root)?;
            let state = directory.join("state");
            snapshot.write(&directory.join("source"), &state)?;
            let outside = directory.join("outside");
            std::fs::rename(directory.join(path), &outside)?;
            std::os::unix::fs::symlink(outside, directory.join(path))?;
            assert!(snapshot.check_harness(&state).is_err());
            Ok(())
        })?;
    }
    Ok(())
}
