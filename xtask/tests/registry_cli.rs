use serde::Deserialize;
use std::{io, path::Path, process::Command};

#[derive(Deserialize)]
struct Metadata {
    packages: Vec<Package>,
}

#[derive(Deserialize)]
struct Package {
    name: String,
    publish: Option<Vec<String>>,
    dependencies: Vec<Dependency>,
    targets: Vec<Target>,
}

#[derive(Deserialize)]
struct Dependency {
    kind: Option<String>,
    source: Option<String>,
}

#[derive(Deserialize)]
struct Target {
    name: String,
    kind: Vec<String>,
}

#[test]
fn the_registry_contains_only_the_standalone_client_command() -> io::Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or_else(|| io::Error::other("workspace root"))?;
    let output = Command::new(env!("CARGO"))
        .current_dir(root)
        .args([
            "metadata",
            "--format-version",
            "1",
            "--locked",
            "--offline",
            "--no-deps",
        ])
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let metadata: Metadata = serde_json::from_slice(&output.stdout)?;
    let publishable: Vec<_> = metadata
        .packages
        .iter()
        .filter(|package| {
            package
                .publish
                .as_ref()
                .is_none_or(|registries| !registries.is_empty())
        })
        .collect();
    let names: Vec<_> = publishable
        .iter()
        .map(|package| package.name.as_str())
        .collect();
    assert_eq!(names, ["rusterisk-cli"]);
    let cli = publishable[0];
    assert_eq!(
        cli.publish.as_deref(),
        Some(["crates-io".to_owned()].as_slice())
    );
    assert!(cli
        .targets
        .iter()
        .any(|target| target.name == "rusterisk-cli" && target.kind == ["bin"]));
    assert!(cli
        .dependencies
        .iter()
        .filter(|dependency| dependency.kind.as_deref() != Some("dev"))
        .all(|dependency| dependency.source.as_deref()
            == Some("registry+https://github.com/rust-lang/crates.io-index")));
    Ok(())
}
