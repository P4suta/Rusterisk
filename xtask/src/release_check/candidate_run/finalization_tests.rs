#![cfg(unix)]

use super::{checksum, combine, conclude, Loaded, RunError, Workspace};
use crate::{
    process::ProcessError,
    release_check::candidate_fixtures::{native_target, require, using},
};
use std::{fs, io};

#[derive(Clone, Copy)]
enum FollowUp {
    Success,
    FinishFailure,
    SourceChange,
    CombinedFailure,
}

fn finalization(case: FollowUp) -> io::Result<()> {
    using(native_target()?, |captured, paths| {
        let loaded = Loaded::read(captured, &paths.server, paths.target, &paths.sdk)
            .map_err(io::Error::other)?;
        let workspace = Workspace::create(
            &paths.execution,
            &paths.consumer_execution,
            &paths.artifacts,
        )
        .map_err(io::Error::other)?;
        let source = workspace.source();
        let copy = captured
            .write(&source, &workspace.state())
            .map_err(|error| RunError::source("finalization fixture source", error));
        if let Err(error) = copy {
            return combine(Err(error), workspace.close()).map_err(io::Error::other);
        }
        let kept = paths
            .consumer_execution
            .with_file_name("kept-owned-consumer");
        let operation = match case {
            FollowUp::CombinedFailure => Err(RunError::contract(
                "native fixture",
                "requested operation failure",
            )),
            FollowUp::Success | FollowUp::FinishFailure | FollowUp::SourceChange => Ok(()),
        };
        let mut finishes = 0;
        let mut ownership_at_finish = false;
        let mut preserved_before_finish = false;
        let result = conclude(operation, &loaded, captured, workspace, || {
            finishes += 1;
            ownership_at_finish = paths.execution.is_dir() && paths.consumer_execution.is_dir();
            preserved_before_finish = paths.artifacts.join("server-candidate").exists()
                || paths.artifacts.join("release-source").exists();
            match case {
                FollowUp::Success => Ok(()),
                FollowUp::FinishFailure => Err(ProcessError::Interrupted),
                FollowUp::SourceChange => {
                    fs::write(
                        source.join("xtask/src/lib.rs"),
                        b"changed during native finish\n",
                    )?;
                    Ok(())
                }
                FollowUp::CombinedFailure => {
                    fs::rename(&paths.consumer_execution, &kept)?;
                    fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&paths.consumer_execution)?;
                    Err(ProcessError::Interrupted)
                }
            }
        });
        let primary = (|| {
            require(
                finishes == 1,
                "Candidate finalization skipped or repeated finish",
            )?;
            require(
                ownership_at_finish,
                "Candidate workspaces closed before native finish",
            )?;
            require(
                !preserved_before_finish,
                "Candidate artifacts were preserved before native finish",
            )?;
            require(
                !paths.execution.exists(),
                "Candidate finalization retained its execution workspace",
            )?;
            match case {
                FollowUp::Success => {
                    result.map_err(io::Error::other)?;
                    require(
                        !paths.consumer_execution.exists(),
                        "Candidate finalization retained its consumer workspace",
                    )?;
                    let archive = paths
                        .artifacts
                        .join("server-candidate")
                        .join(loaded.server.name().file_name());
                    require(
                        checksum(&archive, 272 * 1024 * 1024)? == loaded.server.checksum(),
                        "Candidate preservation changed its verified archive bytes",
                    )?;
                    captured
                        .check(&paths.artifacts.join("release-source"))
                        .map_err(io::Error::other)
                }
                FollowUp::FinishFailure | FollowUp::SourceChange | FollowUp::CombinedFailure => {
                    let message = result
                        .err()
                        .ok_or_else(|| {
                            io::Error::other("Failed candidate finalization returned success")
                        })?
                        .to_string();
                    match case {
                        FollowUp::FinishFailure => require(
                            message.contains("interrupted"),
                            "Candidate finalization lost its native finish failure",
                        )?,
                        FollowUp::SourceChange => require(
                            message.contains("source") && message.contains("changed"),
                            "Candidate finalization checked source before native finish",
                        )?,
                        FollowUp::CombinedFailure => {
                            require(
                                message.contains("requested operation failure"),
                                "Candidate finalization lost its primary failure",
                            )?;
                            require(
                                message.contains("interrupted"),
                                "Candidate finalization lost its simultaneous finish failure",
                            )?;
                            require(message.contains("workspace cleanup"), "Candidate finalization lost its simultaneous filesystem cleanup failure")?;
                            require(paths.consumer_execution.is_file() && kept.is_dir(), "Candidate cleanup removed a replaced path or its preserved owned directory")?;
                        }
                        FollowUp::Success => {
                            return Err(io::Error::other("Invalid failure fixture selection"))
                        }
                    }
                    if !matches!(case, FollowUp::CombinedFailure) {
                        require(
                            !paths.consumer_execution.exists(),
                            "Failed finalization retained its consumer workspace",
                        )?;
                    }
                    Ok(())
                }
            }
        })();
        let cleanup = if matches!(case, FollowUp::CombinedFailure) {
            let file = fs::remove_file(&paths.consumer_execution);
            let directory = fs::remove_dir_all(&kept);
            match (file, directory) {
                (Ok(()), Ok(())) => Ok(()),
                (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
                (Err(first), Err(second)) => Err(io::Error::other(format!(
                    "{first}; fixture directory cleanup: {second}"
                ))),
            }
        } else {
            Ok(())
        };
        match (primary, cleanup) {
            (primary, Ok(())) => primary,
            (Ok(()), Err(error)) => Err(error),
            (Err(primary), Err(cleanup)) => Err(io::Error::other(format!(
                "{primary}; fixture cleanup: {cleanup}"
            ))),
        }
    })
}

#[test]
fn candidate_finalization_preserves_source_bound_bytes_after_native_finish() -> io::Result<()> {
    finalization(FollowUp::Success)
}

#[test]
fn failed_native_finish_still_checks_and_closes_owned_candidate_workspaces() -> io::Result<()> {
    finalization(FollowUp::FinishFailure)
}

#[test]
fn source_changes_during_native_finish_cannot_pass_candidate_finalization() -> io::Result<()> {
    finalization(FollowUp::SourceChange)
}

#[test]
fn primary_finish_and_filesystem_cleanup_failures_remain_identifiable() -> io::Result<()> {
    finalization(FollowUp::CombinedFailure)
}
