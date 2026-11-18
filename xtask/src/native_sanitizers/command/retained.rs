use super::{CapturedNative, CheckedNative, NativeError, NativeOutcome, NativeSanitizerPlan};
use crate::native_sanitizers::{
    native_archives,
    runner::{compiler, output},
    validate_instrumented_archive, validate_rejection, Defect, NativeInventory, NativeLibrary,
    Phase, Scope,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    fs, io,
    path::{Component, Path, PathBuf},
};

const PHASES: [Phase; 19] = [
    Phase::ClangVersion,
    Phase::RustVersion,
    Phase::CompileProbe(Defect::Address),
    Phase::RejectProbe(Defect::Address),
    Phase::CompileProbe(Defect::Undefined),
    Phase::RejectProbe(Defect::Undefined),
    Phase::Fetch,
    Phase::Build(Scope::Media),
    Phase::Build(Scope::Storage),
    Phase::Build(Scope::Tls),
    Phase::InspectArchive(NativeLibrary::Opus),
    Phase::InspectArchive(NativeLibrary::Sqlite),
    Phase::InspectArchive(NativeLibrary::AwsLc),
    Phase::Inventory(Scope::Media),
    Phase::Run(Scope::Media),
    Phase::Inventory(Scope::Storage),
    Phase::Run(Scope::Storage),
    Phase::Inventory(Scope::Tls),
    Phase::Run(Scope::Tls),
];

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NativeExecution {
    identity: String,
    directory: PathBuf,
    architecture: String,
    probe_codes: [i32; 2],
}

fn invalid(reason: &'static str) -> NativeError {
    io::Error::new(io::ErrorKind::InvalidData, reason).into()
}

impl NativeExecution {
    pub(crate) fn check(&self, image: &str) -> Result<NativeSanitizerPlan, NativeError> {
        let directory = self
            .directory
            .to_str()
            .ok_or_else(|| invalid("native execution directory is not UTF-8"))?;
        if !self.directory.is_absolute()
            || directory.len() > 4096
            || directory
                .chars()
                .any(|value| value.is_control() || matches!(value, '\\' | ':' | ','))
            || directory
                .split('/')
                .skip(1)
                .any(|part| part.is_empty() || matches!(part, "." | ".."))
            || self.directory.components().any(|part| {
                matches!(
                    part,
                    Component::Prefix(_) | Component::CurDir | Component::ParentDir
                )
            })
            || self.directory.file_name().and_then(|name| name.to_str())
                != Some(format!("rusterisk-native-sanitizers-{}", self.identity).as_str())
            || !matches!(self.architecture.as_str(), "x86_64" | "aarch64")
        {
            return Err(invalid("native retained execution profile differs"));
        }
        NativeSanitizerPlan::new(
            &self.identity,
            self.directory
                .join("source")
                .to_str()
                .ok_or_else(|| invalid("native source path is not UTF-8"))?,
            self.directory
                .join("state")
                .to_str()
                .ok_or_else(|| invalid("native state path is not UTF-8"))?,
            image,
        )
        .map_err(NativeError::Io)
    }

    pub(super) fn capture(outcome: &NativeOutcome) -> Result<Self, NativeError> {
        let plan = &outcome.checked.plan;
        let source = Path::new(&plan.source);
        let directory = source
            .parent()
            .ok_or_else(|| invalid("native source has no execution parent"))?;
        if source != directory.join("source") || Path::new(&plan.state) != directory.join("state") {
            return Err(invalid("native execution source or state path differs"));
        }
        let execution = Self {
            identity: plan.identity.clone(),
            directory: directory.to_owned(),
            architecture: std::env::consts::ARCH.to_owned(),
            probe_codes: outcome.checked.probe_codes,
        };
        execution.check(plan.image())?;
        Ok(execution)
    }
}

fn expected(capture: &CapturedNative) -> BTreeSet<PathBuf> {
    capture
        .inputs()
        .map(|(path, _)| Path::new("source").join(path))
        .chain(capture.inputs().filter_map(|(path, _)| {
            path.strip_prefix("examples/browser")
                .ok()
                .map(|path| Path::new("captured-browser/browser").join(path))
        }))
        .chain(std::iter::once(PathBuf::from("SOURCE.sha256")))
        .chain(
            PHASES
                .into_iter()
                .map(|phase| PathBuf::from(format!("{}.txt", phase.label()))),
        )
        .collect()
}

fn check_files(root: &Path, relative: &Path, remaining: &mut BTreeSet<PathBuf>) -> io::Result<()> {
    if relative.components().count() > 32 || !fs::symlink_metadata(root.join(relative))?.is_dir() {
        return Err(io::Error::other(
            "native retained directory profile differs",
        ));
    }
    for entry in fs::read_dir(root.join(relative))? {
        let entry = entry?;
        let path = relative.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_dir() {
            if !remaining.iter().any(|file| file.starts_with(&path)) {
                return Err(io::Error::other(
                    "native retained directory inventory differs",
                ));
            }
            check_files(root, &path, remaining)?;
        } else if !kind.is_file() || !remaining.remove(&path) {
            return Err(io::Error::other("native retained file inventory differs"));
        }
    }
    Ok(())
}

fn check_sources(capture: &CapturedNative, root: &Path) -> Result<(), NativeError> {
    let mut remaining = expected(capture);
    check_files(root, Path::new(""), &mut remaining)?;
    if !remaining.is_empty()
        || output(&root.join("SOURCE.sha256"))? != format!("{}\n", capture.digest()).as_bytes()
    {
        return Err(invalid(
            "native retained source or report inventory differs",
        ));
    }
    capture.check_source(&root.join("source"))?;
    for (path, bytes) in capture.inputs() {
        if let Ok(relative) = path.strip_prefix("examples/browser") {
            if output(&root.join("captured-browser/browser").join(relative))? != bytes {
                return Err(invalid("native retained browser input differs"));
            }
        }
    }
    Ok(())
}

fn validate(
    capture: &CapturedNative,
    root: &Path,
    image: &str,
    execution: &NativeExecution,
) -> Result<CheckedNative, NativeError> {
    let plan = execution.check(image)?;
    check_sources(capture, root)?;
    for selected in PHASES {
        let bytes = output(&root.join(format!("{}.txt", selected.label())))?;
        compiler(selected, &bytes, &execution.architecture)?;
    }
    for (defect, code) in Defect::ALL.into_iter().zip(execution.probe_codes) {
        let phase = Phase::RejectProbe(defect);
        validate_rejection(
            defect,
            Some(code),
            &output(&root.join(format!("{}.txt", phase.label())))?,
        )?;
    }
    for archive in native_archives(&output(&root.join("build-tls.txt"))?)? {
        validate_instrumented_archive(&output(
            &root.join(format!("inspect-{}.txt", archive.library().label())),
        )?)?;
    }
    let mut inventories = [None, None, None];
    let mut total = 0usize;
    for (scope, retained) in Scope::ALL.into_iter().zip(&mut inventories) {
        let inventory = NativeInventory::parse(
            scope,
            &output(&root.join(format!("inventory-{}.txt", scope.label())))?,
        )?;
        let count =
            inventory.validate(&output(&root.join(format!("run-{}.txt", scope.label())))?)?;
        total = total
            .checked_add(count)
            .ok_or_else(|| invalid("native retained test count overflowed"))?;
        *retained = Some(inventory);
    }
    let [Some(media), Some(storage), Some(tls)] = inventories else {
        return Err(invalid("native retained scope inventory is incomplete"));
    };
    Ok(CheckedNative {
        plan,
        inventories: [media, storage, tls],
        total,
        probe_codes: execution.probe_codes,
    })
}

impl CapturedNative {
    pub(crate) fn verify_retained(
        self,
        image: &str,
        directory: &Path,
    ) -> Result<(NativeOutcome, NativeExecution), NativeError> {
        let outcome = self.verify(directory, image)?;
        let execution = NativeExecution::capture(&outcome)?;
        validate(&outcome.capture, directory, image, &execution)?;
        Ok((outcome, execution))
    }

    pub(crate) fn read_retained(
        self,
        image: &str,
        directory: &Path,
        execution: &NativeExecution,
    ) -> Result<NativeOutcome, NativeError> {
        let checked = validate(&self, directory, image, execution)?;
        Ok(NativeOutcome {
            capture: self,
            checked,
            artifacts: directory.to_owned(),
        })
    }
}
