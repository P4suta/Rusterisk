use super::{project, write, SourceSnapshot};
use std::{
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

const PATCHED: &[u8] =
    b"[workspace]\nmembers=['xtask']\n[patch.crates-io]\nstr0m={path='vendor/str0m'}\n";

fn fixture(
    name: &str,
    operation: impl FnOnce(&Path, &Path) -> Result<(), std::io::Error>,
) -> Result<(), std::io::Error> {
    let identity = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(std::io::Error::other)?
        .as_nanos();
    let directory = std::env::temp_dir().join(format!(
        "rusterisk-browser-{name}-{}-{identity}",
        std::process::id()
    ));
    std::fs::create_dir(&directory)?;
    let root = directory.join("root");
    let result = (|| {
        project(&root)?;
        operation(&root, &directory)
    })();
    match (result, std::fs::remove_dir_all(directory)) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(primary), Err(cleanup)) => Err(std::io::Error::other(format!(
            "{primary}; fixture cleanup failed: {cleanup}"
        ))),
    }
}

fn vendor(root: &Path) -> Result<(), std::io::Error> {
    write(root, "Cargo.toml", PATCHED)?;
    for file in [
        "vendor/str0m/Cargo.toml",
        "vendor/str0m-source.toml",
        "vendor/str0m.patch",
    ] {
        write(root, file, b"fixture")?;
    }
    Ok(())
}

fn sparse(root: &Path, file: &str, size: u64) -> Result<(), std::io::Error> {
    write(root, file, b"")?;
    std::fs::OpenOptions::new()
        .write(true)
        .open(root.join(file))?
        .set_len(size)
}

#[test]
fn vendored_dependency_is_copied_only_for_the_exact_patch_and_remains_source_bound(
) -> Result<(), std::io::Error> {
    fixture("vendor", |root, directory| {
        vendor(root)?;
        write(root, "vendor/str0m/src/lib.rs", b"fixture")?;
        write(root, "Cargo.toml", b"[workspace]\nmembers=['xtask']\n")?;
        let ignored = SourceSnapshot::read(root).map_err(std::io::Error::other)?;
        ignored
            .write(
                &directory.join("unpatched"),
                &directory.join("unpatched-state"),
            )
            .map_err(std::io::Error::other)?;
        assert!(!directory.join("unpatched/vendor").exists());
        write(root, "Cargo.toml", PATCHED)?;
        let snapshot = SourceSnapshot::read(root).map_err(std::io::Error::other)?;
        assert_ne!(snapshot.digest(), ignored.digest());
        let source = directory.join("source");
        snapshot
            .write(&source, &directory.join("state"))
            .map_err(std::io::Error::other)?;
        snapshot
            .check_original(&source)
            .map_err(std::io::Error::other)?;
        for file in [
            "vendor/str0m/src/lib.rs",
            "vendor/str0m/Cargo.toml",
            "vendor/str0m-source.toml",
            "vendor/str0m.patch",
        ] {
            assert_eq!(std::fs::read(source.join(file))?, b"fixture");
            write(&source, file, b"changed")?;
            assert!(snapshot.check_original(&source).is_err());
            write(&source, file, b"fixture")?;
        }
        write(&source, "vendor/str0m/src/hidden.rs", b"added")?;
        assert!(snapshot.check_original(&source).is_err());
        std::fs::remove_file(source.join("vendor/str0m/src/hidden.rs"))?;
        std::fs::remove_file(source.join("vendor/str0m.patch"))?;
        assert!(snapshot.check_original(&source).is_err());
        for path in ["other/str0m", "../str0m", "/tmp/str0m"] {
            write(
                root,
                "Cargo.toml",
                format!(
                    "[workspace]\nmembers=['xtask']\n[patch.crates-io]\nstr0m={{path='{path}'}}\n"
                )
                .as_bytes(),
            )?;
            assert!(SourceSnapshot::read(root).is_err());
        }
        Ok(())
    })
}

#[test]
fn vendor_limits_do_not_expand_project_files_and_bound_the_tree_and_whole_snapshot(
) -> Result<(), std::io::Error> {
    fixture("vendor-bounds", |root, _| {
        vendor(root)?;
        sparse(root, "vendor/str0m/large", 1024 * 1024 + 1)?;
        assert!(SourceSnapshot::read(root).is_ok());
        let large = std::fs::OpenOptions::new()
            .write(true)
            .open(root.join("vendor/str0m/large"))?;
        large.set_len(8 * 1024 * 1024)?;
        assert!(SourceSnapshot::read(root).is_ok());
        large.set_len(8 * 1024 * 1024 + 1)?;
        assert!(SourceSnapshot::read(root).is_err());
        large.set_len(6 * 1024 * 1024)?;
        sparse(root, "vendor/str0m/second", 6 * 1024 * 1024 - 7)?;
        assert!(SourceSnapshot::read(root).is_ok());
        write(root, "vendor/str0m/excess", b"x")?;
        assert!(SourceSnapshot::read(root).is_err());
        std::fs::remove_file(root.join("vendor/str0m/excess"))?;
        sparse(root, "xtask/src/oversized.rs", 1024 * 1024 + 1)?;
        assert!(SourceSnapshot::read(root).is_err());
        std::fs::remove_file(root.join("xtask/src/oversized.rs"))?;
        for number in 0..27 {
            sparse(root, &format!("xtask/src/part{number}.rs"), 1024 * 1024)?;
        }
        assert!(SourceSnapshot::read(root).is_ok());
        sparse(root, "xtask/src/excess.rs", 1024 * 1024)?;
        assert!(SourceSnapshot::read(root).is_err());
        Ok(())
    })
}

#[cfg(unix)]
#[test]
fn vendor_symlinks_are_refused_for_ancestors_tree_files_and_records() -> Result<(), std::io::Error>
{
    for (name, path) in [
        ("vendor-parent", "vendor"),
        ("vendor-tree", "vendor/str0m"),
        ("vendor-file", "vendor/str0m/Cargo.toml"),
        ("vendor-record", "vendor/str0m-source.toml"),
        ("vendor-patch", "vendor/str0m.patch"),
    ] {
        fixture(name, |root, directory| {
            vendor(root)?;
            let outside = directory.join("outside");
            std::fs::rename(root.join(path), &outside)?;
            std::os::unix::fs::symlink(outside, root.join(path))?;
            assert!(SourceSnapshot::read(root).is_err());
            Ok(())
        })?;
    }
    Ok(())
}
