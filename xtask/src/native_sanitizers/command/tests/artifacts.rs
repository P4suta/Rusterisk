use super::fixture::{using_fixture, IMAGE};
use crate::{
    native_sanitizers::{CapturedNative, NativeError, NativeOutcome, Scope},
    release_check::{CapturedReleaseSource, SourceInputs},
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
    let captured = CapturedReleaseSource::read(root).map_err(io::Error::other)?;
    let native = CapturedNative::read(root).map_err(io::Error::other)?;
    let projection = SourceInputs::new(native.inputs()).map_err(io::Error::other)?;
    captured
        .inputs()
        .and_then(|inputs| inputs.bind(&projection))
        .map_err(io::Error::other)?;
    Ok(captured)
}

#[test]
fn invalid_images_cannot_start_or_publish_retained_native_sanitizers() -> io::Result<()> {
    using_fixture(|fixture| {
        let captured = release_source()?;
        let published = fixture.root.join("published");
        let result: Result<NativeOutcome, NativeError> =
            captured.verify_native_sanitizers_artifact("not-a-digest", &published);
        require(
            result.is_err() && !published.exists(),
            "Invalid sanitizer image selection created a reusable artifact",
        )
    })
}

#[test]
fn retained_native_sanitizers_preserve_existing_publication_destinations() -> io::Result<()> {
    using_fixture(|fixture| {
        let captured = release_source()?;
        let published = fixture.root.join("published");
        fs::create_dir(&published)?;
        let marker = published.join("foreign");
        fs::write(&marker, b"preserve existing publication")?;
        let result: Result<NativeOutcome, NativeError> =
            captured.verify_native_sanitizers_artifact(IMAGE, &published);
        require(
            result.is_err() && fs::read(marker)? == b"preserve existing publication",
            "Retained sanitizer checks changed an existing publication",
        )
    })
}

#[test]
fn missing_native_sanitizer_artifacts_do_not_create_reader_state() -> io::Result<()> {
    using_fixture(|fixture| {
        let captured = release_source()?;
        let missing = fixture.root.join("missing");
        let result: Result<NativeOutcome, NativeError> =
            captured.read_native_sanitizers_artifact(&missing, IMAGE);
        require(
            result.is_err() && !missing.exists(),
            "Sanitizer artifact reader accepted or created missing retained inputs",
        )
    })
}

#[test]
fn a_partial_native_scope_cannot_complete_all_sanitizer_boundaries() -> io::Result<()> {
    using_fixture(|fixture| {
        let captured = release_source()?;
        for scope in Scope::ALL {
            let partial = fixture.root.join(scope.label());
            fs::create_dir(&partial)?;
            let inventory = partial.join(format!("inventory-{}.txt", scope.label()));
            fs::write(&inventory, b"incomplete native inventory\n")?;
            let result: Result<NativeOutcome, NativeError> =
                captured.read_native_sanitizers_artifact(&partial, IMAGE);
            require(
                result.is_err() && fs::read(inventory)? == b"incomplete native inventory\n",
                "Sanitizer artifact reader promoted or rewrote a partial scope",
            )?;
        }
        Ok(())
    })
}

#[test]
fn unpublished_native_diagnostics_cannot_be_imported_as_completed_sanitizers() -> io::Result<()> {
    using_fixture(|fixture| {
        let captured = release_source()?;
        let diagnostic = fixture.root.join(".cache/native-sanitizers/diagnostic");
        fs::create_dir_all(&diagnostic)?;
        let build = diagnostic.join("build-tls.txt");
        let run = diagnostic.join("run-tls.txt");
        fs::write(&build, b"incomplete native build diagnostics\n")?;
        fs::write(&run, b"incomplete native execution diagnostics\n")?;
        let result: Result<NativeOutcome, NativeError> =
            captured.read_native_sanitizers_artifact(&diagnostic, IMAGE);
        require(
            result.is_err()
                && fs::read(build)? == b"incomplete native build diagnostics\n"
                && fs::read(run)? == b"incomplete native execution diagnostics\n",
            "Sanitizer artifact reader promoted or changed unpublished diagnostics",
        )?;
        let entries = fs::read_dir(&diagnostic)?.collect::<Result<Vec<_>, io::Error>>()?;
        require(
            entries.len() == 2,
            "Sanitizer artifact reader added publication state to native diagnostics",
        )
    })
}
