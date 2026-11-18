use crate::{core_assurance::CapturedAssurance, release_check::CapturedReleaseSource};
use std::{
    fs, io,
    path::{Path, PathBuf},
};

mod project_fixture {
    use crate as xtask;
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/sdk_candidate_build/fixture.rs"
    ));
}

pub(super) const IMAGE: &str =
    "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

pub(super) struct Paths {
    pub source: PathBuf,
    pub artifacts: PathBuf,
}

pub(super) fn require(condition: bool, reason: &'static str) -> io::Result<()> {
    if condition {
        Ok(())
    } else {
        Err(io::Error::other(reason))
    }
}

fn prepare(root: &Path) -> io::Result<()> {
    let snapshot =
        CapturedAssurance::read(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../crates/core"))
            .map_err(io::Error::other)?;
    for (path, bytes) in snapshot.inputs() {
        let path = root.join("crates/core").join(path);
        fs::create_dir_all(
            path.parent()
                .ok_or_else(|| io::Error::other("Miri fixture input parent missing"))?,
        )?;
        fs::write(path, bytes)?;
    }
    for path in [
        "clippy.toml",
        "deny.toml",
        "justfile",
        "crates/core/proofs/Cargo.lock",
        "docs/server-distribution.md",
    ] {
        project_fixture::write(root, path, b"Miri release fixture\n")?;
    }
    project_fixture::write(
        root,
        "rust-toolchain.toml",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../rust-toolchain.toml"
        )),
    )?;
    project_fixture::write(
        root,
        "containers/input-verification/Dockerfile",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../containers/input-verification/Dockerfile"
        )),
    )?;
    for path in [
        "api",
        ".github/workflows",
        "tests/interop/node-ari-client",
        "tests/interop/sipp",
        "docs/adr",
    ] {
        fs::create_dir_all(root.join(path))?;
    }
    Ok(())
}

pub(super) fn using(
    check: impl FnOnce(&CapturedReleaseSource, &Paths) -> io::Result<()>,
) -> io::Result<()> {
    project_fixture::using_project("release-miri-artifact", |root, parent| {
        prepare(root)?;
        let captured = CapturedReleaseSource::read(root).map_err(io::Error::other)?;
        let source = fs::canonicalize(parent)?.join("retained-release-source");
        captured
            .write(&source, &parent.join("captured-browser"))
            .map_err(io::Error::other)?;
        check(
            &captured,
            &Paths {
                source,
                artifacts: fs::canonicalize(parent)?.join("miri-artifact"),
            },
        )
        .map_err(Into::into)
    })
    .map_err(io::Error::other)
}
