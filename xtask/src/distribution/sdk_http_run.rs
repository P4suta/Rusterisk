use super::{
    check_sdk_consumer_lock, check_sdk_http_metadata, sdk_consumer_run as io, SdkConsumerAction,
    SdkConsumerPhase, SdkConsumerPlan, SdkConsumerRunError, SdkConsumerScope, SdkConsumerWorkspace,
    SdkContents, SdkHttpTarget, SdkMetadataError, SdkPackage, SdkRuntimeIdentity,
};
use crate::{
    process::{ProcessError, SupervisedCommand},
    recorded_path::{RecordedPath, RecordedPathFormat},
};
use std::{
    num::NonZeroUsize,
    path::{Path, PathBuf},
    time::Duration,
};

pub struct PreparedSdkHttpConsumer<'a> {
    workspace: Option<SdkConsumerWorkspace<'a>>,
    artifacts: PathBuf,
    lock: Vec<u8>,
}

fn invalid(message: &str) -> std::io::Error {
    std::io::Error::other(message)
}

fn check_run_output(directory: &Path) -> Result<(), SdkConsumerRunError> {
    let phase = SdkConsumerPhase::new(SdkConsumerScope::Sdk, SdkConsumerAction::Run);
    let output = io::read(&directory.join(io::stdout(phase)), 4096)?;
    let diagnostic = io::read(&directory.join(io::stderr(phase)), 4096)?;
    if output != b"SDK HTTP candidate consumer passed\n" || !diagnostic.is_empty() {
        return Err(SdkConsumerRunError::Output);
    }
    Ok(())
}

#[cfg(unix)]
pub(crate) fn check_sdk_http_results(
    contents: &SdkContents,
    artifacts: &Path,
    plan: &SdkConsumerPlan,
) -> Result<(), SdkConsumerRunError> {
    check_results(contents, artifacts, |runtime, output| {
        check_sdk_http_metadata(plan, contents.version(), runtime, output)
    })
}

#[cfg(any(unix, windows))]
pub(crate) fn check_sdk_http_results_recorded(
    contents: &SdkContents,
    artifacts: &Path,
    format: RecordedPathFormat,
    consumer_root: &RecordedPath,
) -> Result<(), SdkConsumerRunError> {
    check_results(contents, artifacts, |runtime, output| {
        super::sdk_consumer_metadata::check_recorded(
            format,
            consumer_root,
            SdkConsumerScope::Sdk,
            contents.version(),
            output,
            Some(runtime),
        )
    })
}

#[cfg(any(unix, windows))]
fn check_results(
    contents: &SdkContents,
    artifacts: &Path,
    mut check_metadata: impl FnMut(&SdkRuntimeIdentity, &[u8]) -> Result<(), SdkMetadataError>,
) -> Result<(), SdkConsumerRunError> {
    if io::read(&artifacts.join("SOURCE.sha256"), 65)?
        != format!("{}\n", contents.source_digest()).as_bytes()
    {
        return Err(invalid("HTTP consumer output source differs").into());
    }
    let sums: String = SdkPackage::ALL
        .iter()
        .map(|package| {
            format!(
                "{}  {}-{}.crate\n",
                contents.checksum(*package),
                package.name(),
                contents.version()
            )
        })
        .collect();
    if io::read(&artifacts.join("SHA256SUMS"), 1024)? != sums.as_bytes() {
        return Err(invalid("HTTP consumer candidate archives differ").into());
    }
    let scope = SdkConsumerScope::Sdk;
    let lock = io::read(&artifacts.join("consumers/sdk/Cargo.lock"), 2 * 1024 * 1024)?;
    check_sdk_consumer_lock(scope, contents.version(), io::seed(contents, scope)?, &lock)
        .map_err(SdkConsumerRunError::Lock)?;
    if io::read(
        &artifacts.join("consumers/core-msrv/Cargo.lock"),
        2 * 1024 * 1024,
    )? != io::seed(contents, SdkConsumerScope::CoreMsrv)?
    {
        return Err(invalid("HTTP consumer changed its unused core lock").into());
    }
    let runtime = SdkRuntimeIdentity::from_contents(contents).map_err(std::io::Error::other)?;
    for action in [
        SdkConsumerAction::Resolve,
        SdkConsumerAction::Metadata,
        SdkConsumerAction::Build,
        SdkConsumerAction::Run,
    ] {
        let phase = SdkConsumerPhase::new(scope, action);
        let output = io::read(&artifacts.join(io::stdout(phase)), 8 * 1024 * 1024)?;
        io::read(&artifacts.join(io::stderr(phase)), 8 * 1024 * 1024)?;
        if matches!(
            action,
            SdkConsumerAction::Resolve | SdkConsumerAction::Metadata
        ) {
            check_metadata(&runtime, &output).map_err(SdkConsumerRunError::Metadata)?;
        }
    }
    check_run_output(artifacts)
}

fn prepare(
    contents: &SdkContents,
    workspace: &SdkConsumerWorkspace<'_>,
    mut perform: impl FnMut(SupervisedCommand, Duration) -> Result<(), ProcessError>,
) -> Result<Vec<u8>, SdkConsumerRunError> {
    let scope = SdkConsumerScope::Sdk;
    io::phase(
        workspace,
        SdkConsumerPhase::new(scope, SdkConsumerAction::Resolve),
        &mut perform,
    )?;
    let lock = io::read(
        &workspace.plan().directory(scope).join("Cargo.lock"),
        2 * 1024 * 1024,
    )?;
    check_sdk_consumer_lock(scope, contents.version(), io::seed(contents, scope)?, &lock)
        .map_err(SdkConsumerRunError::Lock)?;
    let metadata = SdkConsumerPhase::new(scope, SdkConsumerAction::Metadata);
    io::phase(workspace, metadata, &mut perform)?;
    io::lock_unchanged(workspace, scope, &lock)?;
    let bytes = io::read(
        &workspace.plan().state().join(io::stdout(metadata)),
        8 * 1024 * 1024,
    )?;
    let runtime = workspace
        .runtime()
        .ok_or_else(|| invalid("HTTP consumer runtime missing"))?;
    check_sdk_http_metadata(workspace.plan(), contents.version(), runtime, &bytes)
        .map_err(SdkConsumerRunError::Metadata)?;
    io::phase(
        workspace,
        SdkConsumerPhase::new(scope, SdkConsumerAction::Build),
        &mut perform,
    )?;
    io::lock_unchanged(workspace, scope, &lock)?;
    Ok(lock)
}

impl<'a> PreparedSdkHttpConsumer<'a> {
    pub fn prepare_with(
        contents: &'a SdkContents,
        execution: &Path,
        artifacts: &Path,
        perform: impl FnMut(SupervisedCommand, Duration) -> Result<(), ProcessError>,
    ) -> Result<Self, SdkConsumerRunError> {
        Self::prepare_with_jobs(
            contents,
            execution,
            artifacts,
            SdkConsumerPlan::DEFAULT_JOBS,
            perform,
        )
    }

    pub fn prepare_with_jobs(
        contents: &'a SdkContents,
        execution: &Path,
        artifacts: &Path,
        jobs: NonZeroUsize,
        perform: impl FnMut(SupervisedCommand, Duration) -> Result<(), ProcessError>,
    ) -> Result<Self, SdkConsumerRunError> {
        io::create_artifacts(contents, execution, artifacts)?;
        let workspace = SdkConsumerWorkspace::create_http_with_jobs(contents, execution, jobs)?;
        match prepare(contents, &workspace, perform) {
            Ok(lock) => Ok(Self {
                workspace: Some(workspace),
                artifacts: artifacts.to_owned(),
                lock,
            }),
            Err(error) => {
                let operation = io::follow_up(
                    Err(error),
                    io::preserve(&workspace, artifacts),
                    "consumer artifact preservation failed",
                );
                io::follow_up(
                    operation,
                    workspace.close(),
                    "consumer working copy cleanup failed",
                )
            }
        }
    }

    pub fn artifacts(&self) -> &Path {
        &self.artifacts
    }

    fn execute(
        &self,
        target: &SdkHttpTarget<'_>,
        mut perform: impl FnMut(SupervisedCommand, Duration) -> Result<(), ProcessError>,
    ) -> Result<(), SdkConsumerRunError> {
        let workspace = self
            .workspace
            .as_ref()
            .ok_or_else(|| invalid("HTTP consumer ownership ended"))?;
        let scope = SdkConsumerScope::Sdk;
        io::lock_unchanged(workspace, scope, &self.lock)?;
        let phase = SdkConsumerPhase::new(scope, SdkConsumerAction::Run);
        io::phase_with_target(workspace, phase, Some(target), &mut perform)?;
        io::lock_unchanged(workspace, scope, &self.lock)?;
        check_run_output(workspace.plan().state())
    }

    pub fn run_with(
        mut self,
        target: &SdkHttpTarget<'_>,
        perform: impl FnMut(SupervisedCommand, Duration) -> Result<(), ProcessError>,
    ) -> Result<PathBuf, SdkConsumerRunError> {
        let operation = self
            .execute(target, perform)
            .map(|()| self.artifacts.clone());
        self.finish(operation)
    }

    pub fn close(mut self) -> Result<(), SdkConsumerRunError> {
        self.finish(Ok(()))
    }

    fn finish<T>(
        &mut self,
        operation: Result<T, SdkConsumerRunError>,
    ) -> Result<T, SdkConsumerRunError> {
        let workspace = self
            .workspace
            .take()
            .ok_or_else(|| invalid("HTTP consumer ownership ended"))?;
        let operation = io::follow_up(
            operation,
            io::preserve(&workspace, &self.artifacts),
            "consumer artifact preservation failed",
        );
        io::follow_up(
            operation,
            workspace.close(),
            "consumer working copy cleanup failed",
        )
    }
}

impl Drop for PreparedSdkHttpConsumer<'_> {
    fn drop(&mut self) {
        if self.workspace.is_none() {
            return;
        }
        if let Err(error) = self.finish(Ok(())) {
            eprintln!("SDK HTTP consumer fallback cleanup failed: {error}");
        }
    }
}
