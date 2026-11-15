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
