use super::fixture::{invoke, require, using, Fixture};
use std::{fs, io, path::Path, process::Output};

const INPUTS: &[u8] = include_bytes!("inputs.toml");

fn text(path: &Path) -> io::Result<&str> {
    path.to_str()
        .ok_or_else(|| io::Error::other("Release CLI fixture path was not UTF-8"))
}

fn rejected(output: &Output, reason: &str) -> io::Result<()> {
    let stderr = String::from_utf8_lossy(&output.stderr);
    require(
        output.status.code() == Some(1),
        &format!("Release CLI changed its rejection status: {stderr}"),
    )?;
    require(
        output.stdout.is_empty(),
        "Rejected release inputs produced success or progress output",
    )?;
    require(
        stderr.contains(reason) && !stderr.contains("panicked"),
        &format!("Release CLI lost rejection reason {reason:?}: {stderr}"),
    )
}

fn rejected_input(fixture: &Fixture, path: &Path, reason: &str) -> io::Result<()> {
    let path = text(path)?;
    for arguments in [
        &["release-check", "verify", "--inputs", path][..],
        &["release-check", "run", "reference", "--inputs", path][..],
    ] {
        let output = invoke(arguments, fixture.root())?;
        rejected(&output, reason)?;
        require(
            String::from_utf8_lossy(&output.stderr).contains(path),
            "Release CLI omitted the selected input path from its diagnostic",
        )?;
    }
    Ok(())
}

#[test]
fn release_execution_rejects_invalid_input_files_before_dispatch() -> io::Result<()> {
    using(|fixture| {
        let missing = fixture.path("missing.toml");
        rejected_input(fixture, &missing, "release-check input")?;
        let directory = fixture.path("directory.toml");
        fs::create_dir(&directory)?;
        rejected_input(
            fixture,
            &directory,
            "release input file must be a regular file",
        )?;
        let encoding = fixture.write("encoding.toml", &[0xff])?;
        rejected_input(fixture, &encoding, "release inputs must be UTF-8")?;
        let oversized = fixture.write("oversized.toml", "x".repeat(64 * 1024 + 1).as_bytes())?;
        rejected_input(
            fixture,
            &oversized,
            "release inputs exceed the 64 KiB byte limit",
        )?;
        let syntax = fixture.write("syntax.toml", b"format = [\n")?;
        rejected_input(fixture, &syntax, "TOML")?;
        let valid = fixture.write("valid.toml", INPUTS)?;
        #[cfg(unix)]
        {
            let link = fixture.path("linked.toml");
            std::os::unix::fs::symlink(&valid, &link)?;
            rejected_input(fixture, &link, "release input file must be a regular file")?;
        }
        require(
            fs::read(valid)? == INPUTS,
            "Rejected release inputs changed the borrowed configuration",
        )
    })
}

#[test]
fn missing_source_precedes_retained_reads_and_fresh_checks() -> io::Result<()> {
    using(|fixture| {
        let input = fixture.write("inputs.toml", INPUTS)?;
        let output = invoke(
            &["release-check", "verify", "--inputs", text(&input)?],
            fixture.root(),
        )?;
        rejected(&output, "release-check captured source:")?;
        let stderr = String::from_utf8_lossy(&output.stderr);
        for unexpected in ["unavailable", "native gate:", "native supervisor"] {
            require(
                !stderr.contains(unexpected),
                "Release CLI crossed source validation before rejecting the operation",
            )?;
        }
        require(
            !fixture.path("missing-source").exists()
                && !fixture.path("candidates").exists()
                && !fixture.path("artifacts").exists(),
            "Missing release source created source, candidate, or artifact state",
        )?;
        require(
            fs::read(input)? == INPUTS,
            "Release CLI changed its selected inputs",
        )
    })
}

#[test]
fn release_execution_rejects_legacy_incomplete_and_trailing_arguments() -> io::Result<()> {
    using(|fixture| {
        let missing = fixture.path("missing.toml");
        let missing = text(&missing)?;
        let unknown = invoke(
            &["release-check", "run", "unknown", "--inputs", missing],
            fixture.root(),
        )?;
        rejected(
            &unknown,
            "release-check gate is not in the required inventory",
        )?;
        for arguments in [
            &["release-check", "--run"][..],
            &["release-check", "verify", "--inputs"][..],
            &["release-check", "run", "fast"][..],
            &["release-check", "run", "fast", "--inputs", missing, "--run"][..],
            &["release-check", "verify", "--inputs", missing, "--passed"][..],
            &[
                "release-check",
                "verify",
                "--inputs",
                missing,
                "--inputs",
                missing,
            ][..],
        ] {
            let output = invoke(arguments, fixture.root())?;
            rejected(&output, "release-check requires explicit runner inputs")?;
        }
        let trailing: [&str; 13] = [
            "release-check",
            "run",
            "fast",
            "--inputs",
            missing,
            "extra",
            "extra",
            "extra",
            "extra",
            "extra",
            "extra",
            "extra",
            "extra",
        ];
        let output = invoke(&trailing, fixture.root())?;
        rejected(&output, "release-check requires explicit runner inputs")?;
        require(
            !Path::new(missing).exists(),
            "Rejected arguments created input state",
        )
    })
}
