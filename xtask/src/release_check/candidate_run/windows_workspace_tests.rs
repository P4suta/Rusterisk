use super::{Paths, Workspace};
use std::{
    fs, io,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

fn checked<T>(result: Result<T, impl std::fmt::Display>) -> io::Result<T> {
    result.map_err(|error| io::Error::other(error.to_string()))
}

fn require(valid: bool, reason: &'static str) -> io::Result<()> {
    if valid {
        Ok(())
    } else {
        Err(io::Error::other(reason))
    }
}

fn combine<T>(operation: io::Result<T>, cleanup: io::Result<()>) -> io::Result<T> {
    match (operation, cleanup) {
        (operation, Ok(())) => operation,
        (Ok(_), Err(error)) => Err(error),
        (Err(operation), Err(cleanup)) => Err(io::Error::other(format!(
            "{operation}; Windows workspace fixture cleanup failed: {cleanup}"
        ))),
    }
}

fn reserve() -> io::Result<PathBuf> {
    let parent = fs::canonicalize(std::env::temp_dir())?;
    let stamp = checked(SystemTime::now().duration_since(UNIX_EPOCH))?.as_nanos();
    for attempt in 0..32 {
        let root = parent.join(format!(
            "rusterisk-windows-candidate-workspace-{}-{stamp}-{attempt}",
            std::process::id()
        ));
        match fs::create_dir(&root) {
            Ok(()) => return Ok(root),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "Windows candidate workspace fixture capacity exceeded",
    ))
}

fn using(operation: impl FnOnce(&Path) -> io::Result<()>) -> io::Result<()> {
    let root = reserve()?;
    let result = operation(&root);
    combine(result, fs::remove_dir_all(root))
}

#[test]
fn canonical_windows_candidate_paths_close_owned_workspaces_and_keep_artifacts() -> io::Result<()> {
    using(|root| {
        let candidate = root.join("candidate");
        fs::create_dir(&candidate)?;
        let paths = checked(Paths::new(&candidate, "windows-workspace"))?;
        for path in [
            &paths.execution,
            &paths.consumer_execution,
            &paths.artifacts,
        ] {
            let parent = path
                .parent()
                .ok_or_else(|| io::Error::other("Windows candidate workspace has no parent"))?;
            require(
                path.is_absolute() && fs::canonicalize(parent)? == parent,
                "Windows candidate paths lost their canonical physical parents",
            )?;
        }
        let workspace = checked(Workspace::create(
            &paths.execution,
            &paths.consumer_execution,
            &paths.artifacts,
        ))?;
        let operation = (|| {
            require(
                workspace.source() == paths.execution.join("source")
                    && workspace.state() == paths.execution.join("state")
                    && workspace.server() == paths.execution.join("server")
                    && workspace.consumer() == paths.consumer_execution.join("consumer"),
                "Windows candidate workspace changed its owned layout",
            )?;
            fs::write(paths.execution.join("execution-marker"), b"owned execution")?;
            fs::write(
                paths.consumer_execution.join("consumer-marker"),
                b"owned consumer",
            )?;
            fs::write(paths.artifacts.join("retained-marker"), b"retained bytes")
        })();
        let closed = checked(workspace.close());
        let preserved = (|| {
            require(
                !paths.execution.try_exists()? && !paths.consumer_execution.try_exists()?,
                "Windows candidate workspace retained owned execution state",
            )?;
            require(
                fs::read(paths.artifacts.join("retained-marker"))? == b"retained bytes",
                "Windows candidate workspace removed retained artifacts",
            )
        })();
        combine(combine(operation, closed), preserved)
    })
}

#[test]
fn overlapping_windows_candidate_workspaces_are_rejected_before_creation() -> io::Result<()> {
    using(|root| {
        let shared = root.join("shared");
        let artifacts = root.join("artifacts");
        let error = Workspace::create(&shared, &shared, &artifacts)
            .err()
            .ok_or_else(|| io::Error::other("Overlapping Windows workspaces were accepted"))?;
        require(
            error
                .to_string()
                .contains("candidate workspaces must not overlap"),
            "Windows workspace overlap failed at another boundary",
        )?;
        require(
            !shared.try_exists()? && !artifacts.try_exists()?,
            "Rejected Windows workspaces created execution or artifact state",
        )
    })
}

#[test]
fn failed_windows_workspace_creation_preserves_foreign_retained_artifacts() -> io::Result<()> {
    using(|root| {
        let execution = root.join("execution");
        let consumer = root.join("consumer");
        let artifacts = root.join("artifacts");
        fs::create_dir(&artifacts)?;
        fs::write(artifacts.join("foreign"), b"existing artifact")?;
        let error = Workspace::create(&execution, &consumer, &artifacts)
            .err()
            .ok_or_else(|| io::Error::other("Windows workspace reused foreign artifacts"))?;
        require(
            error.to_string().contains("candidate workspace creation"),
            "Windows workspace creation lost its failure stage",
        )?;
        require(
            !execution.try_exists()? && !consumer.try_exists()?,
            "Failed Windows workspace creation retained owned directories",
        )?;
        require(
            fs::read(artifacts.join("foreign"))? == b"existing artifact",
            "Failed Windows workspace creation changed foreign artifacts",
        )
    })
}
