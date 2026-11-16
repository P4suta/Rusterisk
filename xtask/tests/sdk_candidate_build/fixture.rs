use std::{
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};
use xtask::distribution::SdkCandidateBuildError;

pub fn using_project(
    label: &str,
    test: impl FnOnce(&Path, &Path) -> Result<(), SdkCandidateBuildError>,
) -> Result<(), SdkCandidateBuildError> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(std::io::Error::other)?
        .as_nanos();
    let parent = std::env::temp_dir().join(format!(
        "rusterisk-sdk-candidate-fixture-{}-{stamp}-{label}",
        std::process::id()
    ));
    std::fs::create_dir(&parent)?;
    let root = parent.join("root");
    let operation = project(&root)
        .map_err(SdkCandidateBuildError::from)
        .and_then(|()| test(&root, &parent));
    let cleanup = std::fs::remove_dir_all(&parent);
    match (operation, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(error)) => Err(error.into()),
        (Err(operation), Err(cleanup)) => Err(std::io::Error::other(format!(
            "{operation}; fixture cleanup failed: {cleanup}"
        ))
        .into()),
    }
}

pub fn write(root: &Path, relative: &str, bytes: &[u8]) -> Result<(), std::io::Error> {
    let path = root.join(relative);
    std::fs::create_dir_all(
        path.parent()
            .ok_or_else(|| std::io::Error::other("fixture parent missing"))?,
    )?;
    std::fs::write(path, bytes)
}

pub fn project(root: &Path) -> Result<(), std::io::Error> {
    write(
        root,
        "Cargo.toml",
        br#"[workspace]
members = ["crates/core", "crates/api", "crates/client", "xtask"]
[workspace.package]
version = "0.1.0"
license = "MIT OR Apache-2.0"
repository = "https://github.com/P4suta/Rusterisk"
"#,
    )?;
    for path in [
        "xtask/Cargo.toml",
        "xtask/fuzz/Cargo.toml",
        "crates/core/assurance/Cargo.toml",
        "crates/core/proofs/Cargo.toml",
    ] {
        write(root, path, b"[package]\nname='fixture'\nversion='0.1.0'\n")?;
    }
    for path in [
        "Cargo.lock",
        "xtask/fuzz/Cargo.lock",
        "xtask/src/lib.rs",
        "LICENSE-MIT",
        "LICENSE-APACHE",
        "docs/rust-sdk.md",
        "mise.toml",
        ".cargo/config.toml",
        "config/http.conf",
        "config/ari.conf",
        "config/pjsip.conf",
        "config/rtp.conf",
        "config/extensions.conf",
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
    for package in ["core", "api", "client"] {
        for file in ["LICENSE-MIT", "LICENSE-APACHE"] {
            write(root, &format!("crates/{package}/{file}"), b"fixture")?;
        }
        let mut manifest = format!(
            "[package]\nname='rusterisk-{package}'\nversion.workspace=true\nlicense.workspace=true\nrepository.workspace=true\ndescription='SDK fixture'\nreadme='../../docs/rust-sdk.md'\n"
        );
        if package == "client" {
            manifest.push_str(
                "exclude=['tests/**','examples/**']\n[dependencies]\nrusterisk-api={version='0.1.0',path='../api'}\nrusterisk-core={version='0.1.0',path='../core'}\n[dev-dependencies]\nrusterisk-server={path='../server'}\nrusterisk-config={path='../config'}\nrusterisk-media={path='../media'}\n",
            );
        }
        write(
            root,
            &format!("crates/{package}/Cargo.toml"),
            manifest.as_bytes(),
        )?;
        write(root, &format!("crates/{package}/src/lib.rs"), b"fixture")?;
    }
    Ok(())
}
