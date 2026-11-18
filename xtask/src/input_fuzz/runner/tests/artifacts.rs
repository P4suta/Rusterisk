use super::fixture::{using_fixture, IMAGE};
use crate::{
    input_fuzz::{validate_inventory, FuzzError, InputFuzzOutcome, LinuxTarget, Target},
    release_check::CapturedReleaseSource,
};
use std::{fs, io, path::Path};

fn require(valid: bool, reason: &'static str) -> io::Result<()> {
    if valid {
        Ok(())
    } else {
        Err(io::Error::other(reason))
    }
}

fn release_source() -> io::Result<CapturedReleaseSource> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or_else(|| io::Error::other("Workspace directory is missing"))?;
    CapturedReleaseSource::read(root).map_err(io::Error::other)
}

#[test]
fn invalid_images_cannot_start_or_publish_retained_input_fuzz_checks() -> io::Result<()> {
    using_fixture(|fixture| {
        let captured = release_source()?;
        for architecture in [LinuxTarget::Arm64, LinuxTarget::Amd64] {
            let published = fixture.root.join(architecture.triple());
            let result: Result<InputFuzzOutcome, FuzzError> =
                captured.verify_input_fuzz_artifact("not-a-digest", architecture, &published);
            require(
                result.is_err() && !published.exists(),
                "Invalid fuzz image selection created a reusable artifact",
            )?;
        }
        Ok(())
    })
}

#[test]
fn retained_input_fuzz_checks_preserve_existing_publication_destinations() -> io::Result<()> {
    using_fixture(|fixture| {
        let captured = release_source()?;
        let published = fixture.root.join("published");
        fs::create_dir(&published)?;
        let marker = published.join("foreign");
        fs::write(&marker, b"preserve existing publication")?;
        for architecture in [LinuxTarget::Arm64, LinuxTarget::Amd64] {
            let result: Result<InputFuzzOutcome, FuzzError> =
                captured.verify_input_fuzz_artifact(IMAGE, architecture, &published);
            require(
                result.is_err() && fs::read(&marker)? == b"preserve existing publication",
                "Retained fuzz checks changed an existing publication",
            )?;
        }
        Ok(())
    })
}

#[test]
fn missing_input_fuzz_artifacts_do_not_create_reader_state() -> io::Result<()> {
    using_fixture(|fixture| {
        let captured = release_source()?;
        for architecture in [LinuxTarget::Arm64, LinuxTarget::Amd64] {
            let missing = fixture.root.join(architecture.triple());
            let result: Result<InputFuzzOutcome, FuzzError> =
                captured.read_input_fuzz_artifact(&missing, IMAGE, architecture);
            require(
                result.is_err() && !missing.exists(),
                "Fuzz artifact reader accepted or created missing retained inputs",
            )?;
        }
        Ok(())
    })
}

#[test]
fn a_partial_fuzz_target_inventory_cannot_complete_the_all_target_artifact() -> io::Result<()> {
    using_fixture(|fixture| {
        let captured = release_source()?;
        let partial = fixture.root.join("partial");
        fs::create_dir(&partial)?;
        let inventory = format!("{}\n", Target::Settings.name());
        require(
            matches!(
                validate_inventory(inventory.as_bytes()),
                Err(FuzzError::Inventory)
            ),
            "A single target unexpectedly satisfied the maintained fuzz inventory",
        )?;
        let path = partial.join("inventory.txt");
        fs::write(&path, inventory.as_bytes())?;
        let result: Result<InputFuzzOutcome, FuzzError> =
            captured.read_input_fuzz_artifact(&partial, IMAGE, LinuxTarget::Arm64);
        require(
            result.is_err() && fs::read(path)? == inventory.as_bytes(),
            "Fuzz artifact reader promoted or rewrote a partial target inventory",
        )
    })
}

#[test]
fn preserved_crash_inputs_cannot_be_imported_as_successful_fuzz_publications() -> io::Result<()> {
    using_fixture(|fixture| {
        let captured = release_source()?;
        let diagnostic = fixture.root.join(".cache/input-fuzz-crashes/diagnostic");
        let target = diagnostic.join(Target::Settings.name());
        fs::create_dir_all(&target)?;
        let crash = target.join("crash-input");
        fs::write(&crash, b"failing input")?;
        for architecture in [LinuxTarget::Arm64, LinuxTarget::Amd64] {
            let result: Result<InputFuzzOutcome, FuzzError> =
                captured.read_input_fuzz_artifact(&diagnostic, IMAGE, architecture);
            require(
                result.is_err() && fs::read(&crash)? == b"failing input",
                "Fuzz artifact reader promoted or changed a preserved crash input",
            )?;
        }
        let entries = fs::read_dir(&diagnostic)?.collect::<Result<Vec<_>, io::Error>>()?;
        require(
            entries.len() == 1,
            "Fuzz artifact reader added publication state to crash diagnostics",
        )
    })
}
