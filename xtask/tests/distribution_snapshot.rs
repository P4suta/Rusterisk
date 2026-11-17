use std::{
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};
use xtask::distribution::SourceSnapshot;

fn write(root: &Path, relative: &str, bytes: &[u8]) -> Result<(), std::io::Error> {
    let path = root.join(relative);
    std::fs::create_dir_all(
        path.parent()
            .ok_or_else(|| std::io::Error::other("fixture parent missing"))?,
    )?;
    std::fs::write(path, bytes)
}

fn project(root: &Path) -> Result<(), std::io::Error> {
    write(root, "Cargo.toml", b"[workspace]\nmembers=['xtask']\n")?;
    for path in [
        "Cargo.lock",
        "xtask/Cargo.toml",
        "xtask/src/lib.rs",
        "xtask/tests/contract.rs",
        "xtask/fuzz/Cargo.toml",
        "xtask/fuzz/Cargo.lock",
    ] {
        write(root, path, b"fixture")?;
    }
    for path in [
        "package.json",
        "package-lock.json",
        "tsconfig.json",
        "eslint.config.mjs",
        "biome.json",
        "playwright.config.ts",
        "index.html",
        "style.css",
        "src/main.ts",
        "tests/call.spec.ts",
        "config/http.conf",
        "config/ari.conf",
        "config/pjsip.conf",
        "config/rtp.conf",
        "config/extensions.conf",
    ] {
        write(root, &format!("examples/browser/{path}"), b"fixture")?;
    }
    for path in [
        "LICENSE-MIT",
        "LICENSE-APACHE",
        "docs/rust-sdk.md",
        "mise.toml",
        ".cargo/config.toml",
        "crates/core/assurance/Cargo.toml",
        "crates/core/proofs/Cargo.toml",
        "config/http.conf",
        "config/ari.conf",
        "config/pjsip.conf",
        "config/rtp.conf",
        "config/extensions.conf",
    ] {
        write(root, path, b"fixture")?;
    }
    for package in ["core", "api", "client"] {
        for file in ["LICENSE-MIT", "LICENSE-APACHE"] {
            write(root, &format!("crates/{package}/{file}"), b"fixture")?;
        }
    }
    Ok(())
}

fn using_project(
    label: &str,
    test: impl FnOnce(&Path, &Path) -> Result<(), std::io::Error>,
) -> Result<(), std::io::Error> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(std::io::Error::other)?
        .as_nanos();
    let parent = std::env::temp_dir().join(format!(
        "rusterisk-distribution-snapshot-{}-{stamp}-{label}",
        std::process::id()
    ));
    std::fs::create_dir(&parent)?;
    let root = parent.join("root");
    let operation = project(&root).and_then(|()| test(&root, &parent));
    let cleanup = std::fs::remove_dir_all(parent);
    match (operation, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(operation), Err(cleanup)) => Err(std::io::Error::other(format!(
            "{operation}; fixture cleanup failed: {cleanup}"
        ))),
    }
}

#[test]
fn distribution_snapshot_binds_source_licenses_documentation_and_tools(
) -> Result<(), std::io::Error> {
    using_project("copy", |root, parent| {
        let snapshot = SourceSnapshot::read(root).map_err(std::io::Error::other)?;
        assert_eq!(snapshot.digest().len(), 64);
        assert!(snapshot
            .files()
            .any(|(path, bytes)| path == Path::new("docs/rust-sdk.md") && bytes == b"fixture"));
        let source = parent.join("source");
        snapshot
            .write(&source, &parent.join("state"))
            .map_err(std::io::Error::other)?;
        assert_eq!(
            std::fs::read(source.join("crates/client/LICENSE-MIT"))?,
            b"fixture"
        );
        write(root, "docs/rust-sdk.md", b"live checkout changed")?;
        write(root, "xtask/src/lib.rs", b"live source changed")?;
        snapshot.check(&source).map_err(std::io::Error::other)?;
        write(&source, "docs/rust-sdk.md", b"captured source changed")?;
        assert!(snapshot.check(&source).is_err());
        write(&source, "docs/rust-sdk.md", b"fixture")?;
        write(&source, "xtask/src/added.rs", b"fixture")?;
        assert!(snapshot.check(&source).is_err());
        Ok(())
    })
}

#[test]
fn distribution_snapshot_refuses_missing_oversize_and_redirected_inputs(
) -> Result<(), std::io::Error> {
    using_project("invalid", |root, _parent| {
        std::fs::remove_file(root.join("docs/rust-sdk.md"))?;
        assert!(SourceSnapshot::read(root).is_err());
        write(root, "docs/rust-sdk.md", &[b'x'; 65537])?;
        assert!(SourceSnapshot::read(root).is_err());
        write(root, "docs/rust-sdk.md", b"fixture")?;
        write(root, "crates/client/NOTICE", b"uncaptured package input")?;
        assert!(SourceSnapshot::read(root).is_err());
        std::fs::remove_file(root.join("crates/client/NOTICE"))?;
        #[cfg(unix)]
        {
            std::fs::rename(root.join("docs"), _parent.join("outside"))?;
            std::os::unix::fs::symlink(_parent.join("outside"), root.join("docs"))?;
            assert!(SourceSnapshot::read(root).is_err());
        }
        Ok(())
    })
}

#[test]
fn distribution_snapshot_combines_the_project_and_package_file_count_limits(
) -> Result<(), std::io::Error> {
    using_project("files", |root, _| {
        let initial = SourceSnapshot::read(root).map_err(std::io::Error::other)?;
        let extra = 2048usize
            .checked_sub(initial.files().count())
            .ok_or_else(|| std::io::Error::other("fixture file inventory too large"))?;
        for index in 0..extra {
            write(root, &format!("xtask/src/input-{index}.rs"), b"x")?;
        }
        let boundary = SourceSnapshot::read(root).map_err(std::io::Error::other)?;
        assert_eq!(boundary.files().count(), 2048);
        write(root, "xtask/src/excess.rs", b"x")?;
        let error = SourceSnapshot::read(root)
            .err()
            .ok_or_else(|| std::io::Error::other("oversize file inventory passed"))?;
        assert!(error.to_string().contains("2049"), "{error}");
        assert!(error.to_string().contains("files"), "{error}");
        Ok(())
    })
}

#[test]
fn distribution_snapshot_combines_the_project_and_package_byte_limits() -> Result<(), std::io::Error>
{
    using_project("bytes", |root, _| {
        let initial = SourceSnapshot::read(root).map_err(std::io::Error::other)?;
        let initial_bytes: usize = initial.files().map(|(_, bytes)| bytes.len()).sum();
        let full = [b'x'; 1024 * 1024];
        for index in 0..39 {
            write(root, &format!("xtask/src/large-{index}.rs"), &full)?;
        }
        let remaining = (1024 * 1024usize)
            .checked_sub(initial_bytes)
            .and_then(|value| value.checked_add(1))
            .ok_or_else(|| std::io::Error::other("fixture byte inventory too large"))?;
        write(root, "xtask/src/last.rs", &full[..remaining])?;
        assert!(SourceSnapshot::read(root).is_err());
        Ok(())
    })
}
