use std::process::Command;

#[test]
fn proof_dry_runs_require_an_immutable_image_and_never_contact_the_daemon(
) -> Result<(), std::io::Error> {
    let project = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or_else(|| std::io::Error::other("workspace missing"))?;
    let image = "registry.example/core-proofs@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["core-proofs", "--dry-run", "--image", image])
        .env("PATH", "")
        .current_dir(project)
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    for expected in [
        "Kani 0.68.0",
        "budget=900s",
        "--cpus=2",
        "--memory=2g",
        "--network=none",
        "18 harnesses",
        image,
        "docker rm --force",
    ] {
        assert!(stdout.contains(expected), "{expected}");
    }
    for arguments in [
        ["core-proofs", "--run", "--image", "latest"],
        ["core-proofs", "--dry-run", "--image", "latest"],
    ] {
        let rejected = Command::new(env!("CARGO_BIN_EXE_xtask"))
            .args(arguments)
            .env("PATH", "")
            .current_dir(project)
            .output()?;
        assert!(!rejected.status.success());
    }
    let rejected = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["core-proofs", "--dry-run", "--image", image, "--run"])
        .env("PATH", "")
        .current_dir(project)
        .output()?;
    assert!(!rejected.status.success());
    Ok(())
}
