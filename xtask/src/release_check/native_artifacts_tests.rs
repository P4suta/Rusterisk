use super::candidate_run::{combine, RunError};
use super::native_artifacts::{NativeArtifactPublication, PublishedNativeArtifact};
use std::{
    fs, io,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

const OUTPUTS: [&str; 2] = ["server/version.stdout.txt", "server/server.stdout.txt"];
const VERSION: &[u8] = b"rusterisk 0.1.0\n";
const RUNTIME: &[u8] = b"native command output\n";

mod rejections;

struct Fixture {
    root: PathBuf,
    staging: PathBuf,
    published: PathBuf,
}

fn require(condition: bool, message: &'static str) -> io::Result<()> {
    if condition {
        Ok(())
    } else {
        Err(io::Error::other(message))
    }
}

fn using(check: impl FnOnce(&Fixture) -> io::Result<()>) -> io::Result<()> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    let root = fs::canonicalize(std::env::temp_dir())?.join(format!(
        "rusterisk-native-artifact-{}-{timestamp}",
        std::process::id()
    ));
    fs::create_dir(&root)?;
    let fixture = Fixture {
        staging: root.join("staging"),
        published: root.join("published"),
        root,
    };
    let operation = check(&fixture);
    match (operation, fs::remove_dir_all(&fixture.root)) {
        (operation, Ok(())) => operation,
        (Ok(()), Err(cleanup)) => Err(cleanup),
        (Err(operation), Err(cleanup)) => Err(io::Error::other(format!(
            "{operation}; native artifact fixture cleanup: {cleanup}"
        ))),
    }
}

fn outputs(root: &Path) -> io::Result<()> {
    fs::create_dir(root.join("server"))?;
    fs::write(root.join(OUTPUTS[0]), VERSION)?;
    fs::write(root.join(OUTPUTS[1]), RUNTIME)
}

#[test]
fn native_outputs_are_published_only_after_owned_cleanup() -> io::Result<()> {
    using(|fixture| {
        let publication = NativeArtifactPublication::create(&fixture.staging, &fixture.published)?;
        outputs(publication.staging())?;
        let working = fixture.root.join("working");
        fs::create_dir(&working)?;
        fs::write(working.join("owned-state"), b"owned execution state\n")?;
        let operation = fs::read(fixture.staging.join(OUTPUTS[0]))
            .map_err(|error| RunError::io("native fixture output", error));
        let cleanup = (|| {
            require(
                !fixture.published.exists(),
                "Native outputs became reusable before cleanup completed",
            )?;
            fs::remove_dir_all(&working)?;
            Ok(())
        })()
        .map_err(|error| RunError::io("native fixture cleanup", error));
        let (artifact, output) =
            publication.publish_completed(combine(operation, cleanup).map_err(io::Error::other))?;
        require(
            output == VERSION,
            "Native publication changed its completed value",
        )?;
        require(
            !working.exists() && !fixture.staging.exists(),
            "Native publication retained execution or staging state",
        )?;
        require(
            artifact.directory() == fixture.published
                && fs::read(artifact.directory().join(OUTPUTS[0]))? == VERSION
                && fs::read(artifact.directory().join(OUTPUTS[1]))? == RUNTIME,
            "Native publication changed its execution outputs",
        )?;
        PublishedNativeArtifact::read(&fixture.published, &OUTPUTS)?;
        Ok(())
    })
}

#[test]
fn cleanup_failure_preserves_diagnostics_without_publishing() -> io::Result<()> {
    using(|fixture| {
        let publication = NativeArtifactPublication::create(&fixture.staging, &fixture.published)?;
        outputs(publication.staging())?;
        let operation = fs::read(fixture.staging.join(OUTPUTS[0]))
            .map_err(|error| RunError::io("native fixture output", error));
        let cleanup = Err(RunError::io(
            "native fixture cleanup",
            io::Error::other("owned cleanup failed"),
        ));
        let result =
            publication.publish_completed(combine(operation, cleanup).map_err(io::Error::other));
        let error = result
            .err()
            .ok_or_else(|| io::Error::other("Failed cleanup returned a reusable artifact"))?;
        require(
            error.to_string().contains("owned cleanup failed"),
            "Native publication lost its cleanup failure",
        )?;
        require(
            !fixture.published.exists(),
            "Failed cleanup published reusable native outputs",
        )?;
        require(
            fs::read(fixture.staging.join(OUTPUTS[1]))? == RUNTIME,
            "Failed cleanup discarded its native diagnostics",
        )
    })
}

#[test]
fn native_failure_cannot_publish_complete_looking_outputs() -> io::Result<()> {
    using(|fixture| {
        let publication = NativeArtifactPublication::create(&fixture.staging, &fixture.published)?;
        outputs(publication.staging())?;
        let working = fixture.root.join("working");
        fs::create_dir(&working)?;
        let operation: Result<Vec<u8>, RunError> = Err(RunError::io(
            "native fixture output",
            io::Error::other("native execution failed"),
        ));
        let cleanup =
            fs::remove_dir(&working).map_err(|error| RunError::io("native fixture cleanup", error));
        let result =
            publication.publish_completed(combine(operation, cleanup).map_err(io::Error::other));
        let error = result
            .err()
            .ok_or_else(|| io::Error::other("Failed execution returned a reusable artifact"))?;
        require(
            !working.exists() && error.to_string().contains("native execution failed"),
            "Native failure skipped cleanup or lost its primary error",
        )?;
        require(
            !fixture.published.exists() && fs::read(fixture.staging.join(OUTPUTS[0]))? == VERSION,
            "Native failure published outputs or discarded diagnostics",
        )
    })
}

#[test]
fn native_failure_keeps_its_primary_before_cleanup_failure() -> io::Result<()> {
    using(|fixture| {
        let publication = NativeArtifactPublication::create(&fixture.staging, &fixture.published)?;
        outputs(publication.staging())?;
        let operation: Result<Vec<u8>, RunError> = Err(RunError::io(
            "native fixture output",
            io::Error::other("native execution failed"),
        ));
        let cleanup = Err(RunError::io(
            "native fixture cleanup",
            io::Error::other("owned cleanup failed"),
        ));
        let result =
            publication.publish_completed(combine(operation, cleanup).map_err(io::Error::other));
        let error = result
            .err()
            .ok_or_else(|| io::Error::other("Failed execution returned a reusable artifact"))?;
        let message = error.to_string();
        let primary = message
            .find("native execution failed")
            .ok_or_else(|| io::Error::other("Native failure lost its primary error"))?;
        let cleanup = message
            .find("owned cleanup failed")
            .ok_or_else(|| io::Error::other("Native failure lost its cleanup error"))?;
        require(primary < cleanup, "Native failure reordered its errors")?;
        require(
            !fixture.published.exists(),
            "Combined native and cleanup failures published outputs",
        )
    })
}

#[test]
fn published_reader_rejects_partial_native_output_trees() -> io::Result<()> {
    using(|fixture| {
        fs::create_dir(&fixture.published)?;
        fs::create_dir(fixture.published.join("server"))?;
        for (present, bytes) in [(OUTPUTS[0], VERSION), (OUTPUTS[1], RUNTIME)] {
            let path = fixture.published.join(present);
            fs::write(&path, bytes)?;
            require(
                PublishedNativeArtifact::read(&fixture.published, &OUTPUTS).is_err(),
                "Native reader accepted an incomplete output inventory",
            )?;
            fs::remove_file(path)?;
        }
        Ok(())
    })
}
