use super::{verify_sdk_with, SdkPort};
use crate::{
    distribution::{
        CapturedSdk, SdkConsumerAction, SdkConsumerPhase, SdkConsumerPlan, SdkConsumerScope,
        SdkContents, SdkPackage,
    },
    process::{ProcessError, SupervisedCommand},
    release_check::{CapturedReleaseSource, SourceInputs},
};
use std::{
    fs, io,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

#[path = "sdk_tests/reusable_artifacts.rs"]
mod reusable_artifacts;

mod archive_fixture {
    use crate as xtask;
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/sdk_content/fixture.rs"
    ));
}
mod captured_fixture {
    use crate as xtask;
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/sdk_candidate_build/fixture.rs"
    ));
}
mod candidate_fixture {
    use crate as xtask;
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/sdk_contents/fixture.rs"
    ));
}
mod metadata_fixture {
    use crate as xtask;
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/sdk_consumer_metadata/fixture.rs"
    ));
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Fault {
    None,
    Phase(SdkConsumerPhase),
    TimedOut,
    Interrupted,
    WrongLock,
    CheckoutMetadata,
    ForeignPlanMetadata,
    DetachedGraph,
    BackendDependency,
    WrongCompletion,
    Finish,
    PhaseAndFinish,
    ChangedDuringFinish,
    Preserve,
    PhaseAndPreserve,
    Cleanup,
    PhaseAndCleanup,
}

struct Fixture {
    candidate: PathBuf,
    execution: PathBuf,
    artifacts: PathBuf,
    contents: SdkContents,
    phases: Vec<SdkConsumerPhase>,
    finishes: usize,
    fault: Fault,
}

struct Port<'a> {
    fixture: &'a mut Fixture,
}

fn require(valid: bool, reason: &'static str) -> io::Result<()> {
    if valid {
        Ok(())
    } else {
        Err(io::Error::other(reason))
    }
}

fn core_phase(action: SdkConsumerAction) -> SdkConsumerPhase {
    SdkConsumerPhase::new(SdkConsumerScope::CoreMsrv, action)
}

fn sdk_phase(action: SdkConsumerAction) -> SdkConsumerPhase {
    SdkConsumerPhase::new(SdkConsumerScope::Sdk, action)
}

fn resolved_lock(scope: SdkConsumerScope) -> String {
    let selected: &[SdkPackage] = match scope {
        SdkConsumerScope::CoreMsrv => &[SdkPackage::Core],
        SdkConsumerScope::Sdk => &SdkPackage::ALL,
    };
    let mut lock = "version=4\n".to_owned();
    for package in selected {
        lock.push_str(&format!(
            "[[package]]\nname='{}'\nversion='0.1.0'\n",
            package.name()
        ));
    }
    lock.push_str(&format!(
        "[[package]]\nname='{}'\nversion='0.0.0'\n",
        scope.package_name()
    ));
    lock
}

impl Fixture {
    fn plan(&self) -> io::Result<SdkConsumerPlan> {
        SdkConsumerPlan::new(&self.execution.join("work"), &self.execution.join("state"))
    }

    fn check_command(&self, command: &Command, budget: Duration) -> io::Result<SdkConsumerPhase> {
        let phase = SdkConsumerPhase::ALL
            .get(self.phases.len())
            .copied()
            .ok_or_else(|| io::Error::other("SDK execution repeated a phase"))?;
        let plan = self.plan()?;
        let expected = plan.step(phase);
        require(
            command.get_program() == plan.program(phase),
            "SDK execution selected another program",
        )?;
        require(
            command.get_current_dir() == Some(plan.directory(phase.scope())),
            "SDK execution escaped its consumer directory",
        )?;
        require(
            command
                .get_args()
                .eq(expected.arguments.iter().map(std::ffi::OsStr::new)),
            "SDK execution changed its phase arguments",
        )?;
        require(
            budget == expected.budget,
            "SDK execution changed its bounded phase budget",
        )?;
        for package in SdkPackage::ALL {
            for file in self.contents.archive(package).files() {
                if file.path().as_str() == "src/lib.rs" {
                    let path = plan
                        .work()
                        .join("packages")
                        .join(format!("{}-{}", package.name(), self.contents.version()))
                        .join("src/lib.rs");
                    require(
                        fs::read(path)? == file.bytes(),
                        "SDK execution did not use the immutable candidate archive",
                    )?;
                }
            }
        }
        Ok(phase)
    }

    fn metadata(&self, plan: &SdkConsumerPlan, scope: SdkConsumerScope) -> io::Result<Vec<u8>> {
        if self.fault == Fault::ForeignPlanMetadata {
            return metadata_fixture::metadata(&metadata_fixture::plan()?, scope).bytes();
        }
        let mut data = metadata_fixture::metadata(plan, scope);
        match self.fault {
            Fault::CheckoutMetadata => {
                data.package("rusterisk-core")?.manifest_path =
                    metadata_fixture::checkout(plan).join("crates/core/Cargo.toml");
            }
            Fault::DetachedGraph => {
                let root = data.node(scope.package_name())?;
                root.dependencies.clear();
                root.deps.clear();
            }
            Fault::BackendDependency => {
                data.add_external(plan, 0)?;
                data.package("registry-package-0")?.name = "rusterisk-server".into();
            }
            Fault::None
            | Fault::Phase(_)
            | Fault::TimedOut
            | Fault::Interrupted
            | Fault::WrongLock
            | Fault::ForeignPlanMetadata
            | Fault::WrongCompletion
            | Fault::Finish
            | Fault::PhaseAndFinish
            | Fault::ChangedDuringFinish
            | Fault::Preserve
            | Fault::PhaseAndPreserve
            | Fault::Cleanup
            | Fault::PhaseAndCleanup => {}
        }
        data.bytes()
    }

    fn write_native(&self, phase: SdkConsumerPhase) -> io::Result<()> {
        let plan = self.plan()?;
        let suffix = match phase.action() {
            SdkConsumerAction::Resolve | SdkConsumerAction::Metadata => "json",
            SdkConsumerAction::Build | SdkConsumerAction::Run => "txt",
        };
        let output = plan
            .state()
            .join(format!("{}.stdout.{suffix}", phase.label()));
        let bytes = match phase.action() {
            SdkConsumerAction::Resolve => {
                let mut lock = resolved_lock(phase.scope());
                if self.fault == Fault::WrongLock {
                    lock = lock.replace("version='0.1.0'", "version='9.9.9'");
                }
                fs::write(plan.directory(phase.scope()).join("Cargo.lock"), lock)?;
                metadata_fixture::metadata(&plan, phase.scope()).bytes()?
            }
            SdkConsumerAction::Metadata => self.metadata(&plan, phase.scope())?,
            SdkConsumerAction::Build => Vec::new(),
            SdkConsumerAction::Run => {
                let text = if self.fault == Fault::WrongCompletion {
                    "incomplete candidate execution\n"
                } else {
                    match phase.scope() {
                        SdkConsumerScope::CoreMsrv => "core candidate consumer passed\n",
                        SdkConsumerScope::Sdk => "SDK candidate consumer passed\n",
                    }
                };
                text.as_bytes().to_vec()
            }
        };
        fs::write(output, bytes)
    }

    fn fail_phase(&self, phase: SdkConsumerPhase) -> Option<ProcessError> {
        match self.fault {
            Fault::Phase(selected) if phase == selected => {
                Some(io::Error::other("fixture SDK phase failure").into())
            }
            Fault::TimedOut if phase == sdk_phase(SdkConsumerAction::Build) => {
                Some(ProcessError::Timeout)
            }
            Fault::Interrupted if phase == sdk_phase(SdkConsumerAction::Run) => {
                Some(ProcessError::Interrupted)
            }
            Fault::PhaseAndFinish | Fault::PhaseAndPreserve | Fault::PhaseAndCleanup
                if phase == core_phase(SdkConsumerAction::Build) =>
            {
                Some(io::Error::other("fixture SDK phase failure").into())
            }
            Fault::None
            | Fault::Phase(_)
            | Fault::TimedOut
            | Fault::Interrupted
            | Fault::WrongLock
            | Fault::CheckoutMetadata
            | Fault::ForeignPlanMetadata
            | Fault::DetachedGraph
            | Fault::BackendDependency
            | Fault::WrongCompletion
            | Fault::Finish
            | Fault::PhaseAndFinish
            | Fault::ChangedDuringFinish
            | Fault::Preserve
            | Fault::PhaseAndPreserve
            | Fault::Cleanup
            | Fault::PhaseAndCleanup => None,
        }
    }
}

impl SdkPort for Port<'_> {
    fn run(&mut self, command: SupervisedCommand, budget: Duration) -> Result<(), ProcessError> {
        let command = command.into_std();
        let phase = self.fixture.check_command(&command, budget)?;
        self.fixture.phases.push(phase);
        self.fixture.write_native(phase)?;
        if let Some(error) = self.fixture.fail_phase(phase) {
            fs::write(
                self.fixture
                    .plan()?
                    .state()
                    .join(format!("{}.stderr.txt", phase.label())),
                b"fixture SDK phase diagnostic\n",
            )?;
            return Err(error);
        }
        Ok(())
    }

    fn finish(self) -> Result<(), ProcessError> {
        self.fixture.finishes += 1;
        require(
            self.fixture.finishes == 1,
            "SDK supervision finished more than once",
        )?;
        if !self.fixture.phases.is_empty() {
            require(
                self.fixture.execution.is_dir(),
                "SDK working copy was deleted before supervisor cleanup",
            )?;
            require(
                self.fixture.plan()?.work().is_dir(),
                "SDK source was deleted before supervisor cleanup",
            )?;
        }
        match self.fixture.fault {
            Fault::Finish | Fault::PhaseAndFinish => {
                return Err(io::Error::other("fixture SDK supervisor failure").into())
            }
            Fault::ChangedDuringFinish => {
                fs::write(
                    self.fixture
                        .plan()?
                        .work()
                        .join("packages/rusterisk-core-0.1.0/src/lib.rs"),
                    b"source changed during SDK finish\n",
                )?;
            }
            Fault::Preserve | Fault::PhaseAndPreserve => {
                fs::write(
                    self.fixture.artifacts.join("core-msrv-resolve.stdout.json"),
                    b"owned preservation collision\n",
                )?;
            }
            Fault::Cleanup | Fault::PhaseAndCleanup => {
                fs::rename(
                    &self.fixture.execution,
                    self.fixture.execution.with_extension("retained"),
                )?;
                fs::write(&self.fixture.execution, b"owned cleanup obstruction\n")?;
            }
            Fault::None
            | Fault::Phase(_)
            | Fault::TimedOut
            | Fault::Interrupted
            | Fault::WrongLock
            | Fault::CheckoutMetadata
            | Fault::ForeignPlanMetadata
            | Fault::DetachedGraph
            | Fault::BackendDependency
            | Fault::WrongCompletion => {}
        }
        Ok(())
    }
}

fn release_inputs(source: &Path) -> io::Result<()> {
    for path in [
        "rust-toolchain.toml",
        "clippy.toml",
        "deny.toml",
        "justfile",
        ".gitignore",
        "crates/core/proofs/Cargo.lock",
        "crates/core/assurance/Cargo.lock",
        "docs/server-distribution.md",
    ] {
        captured_fixture::write(source, path, b"release fixture\n")?;
    }
    for path in [
        "api",
        "containers",
        ".github/workflows",
        "tests/interop/node-ari-client",
        "tests/interop/sipp",
        "docs/adr",
    ] {
        fs::create_dir_all(source.join(path))?;
    }
    Ok(())
}

fn update_source_checksum(candidate: &Path) -> io::Result<()> {
    let source = CapturedSdk::read(&candidate.join("source")).map_err(io::Error::other)?;
    fs::write(
        candidate.join("SOURCE.sha256"),
        format!("{}\n", source.source_digest()),
    )
}

fn using_fixture(
    test: impl FnOnce(&CapturedReleaseSource, &mut Fixture) -> io::Result<()>,
) -> io::Result<()> {
    candidate_fixture::using_candidate(
        "release-sdk-flow",
        |_| Ok(()),
        |candidate| {
            release_inputs(&candidate.join("source"))?;
            update_source_checksum(candidate)?;
            let captured =
                CapturedReleaseSource::read(&candidate.join("source")).map_err(io::Error::other)?;
            let contents = SdkContents::read(candidate).map_err(io::Error::other)?;
            let mut fixture = Fixture {
                candidate: candidate.to_owned(),
                execution: candidate.with_file_name("consumer-execution"),
                artifacts: candidate.with_file_name("consumer-artifacts"),
                contents,
                phases: Vec::new(),
                finishes: 0,
                fault: Fault::None,
            };
            test(&captured, &mut fixture)
        },
    )
}

fn failure(captured: &CapturedReleaseSource, fixture: &mut Fixture) -> io::Result<String> {
    let candidate = fixture.candidate.clone();
    let execution = fixture.execution.clone();
    let artifacts = fixture.artifacts.clone();
    let result = verify_sdk_with(
        captured,
        &candidate,
        &execution,
        &artifacts,
        Port { fixture },
    );
    let error = match result {
        Ok(_) => {
            return Err(io::Error::other(
                "SDK verification returned an outcome after a failed contract",
            ))
        }
        Err(error) => error.to_string(),
    };
    require(
        fixture.finishes == 1,
        "Failed SDK verification skipped supervisor cleanup",
    )?;
    if !matches!(fixture.fault, Fault::Cleanup | Fault::PhaseAndCleanup) {
        require(
            !execution.exists(),
            "Failed SDK verification retained its working copy",
        )?;
    }
    Ok(error)
}

#[test]
fn sdk_outcomes_require_matching_archives_all_eight_native_phases_and_owned_cleanup(
) -> io::Result<()> {
    using_fixture(|captured, fixture| {
        let source =
            CapturedSdk::read(&fixture.candidate.join("source")).map_err(io::Error::other)?;
        let projection = SourceInputs::new(source.files()).map_err(io::Error::other)?;
        let expected = captured
            .inputs()
            .map_err(io::Error::other)?
            .bind(&projection)
            .map_err(io::Error::other)?;
        let candidate = fixture.candidate.clone();
        let execution = fixture.execution.clone();
        let artifacts = fixture.artifacts.clone();
        let outcome = verify_sdk_with(
            captured,
            &candidate,
            &execution,
            &artifacts,
            Port { fixture },
        )
        .map_err(io::Error::other)?;
        require(
            std::ptr::eq(outcome.captured, captured),
            "SDK outcome changed its release source",
        )?;
        require(
            outcome.projection == expected,
            "SDK outcome changed its bound candidate projection",
        )?;
        require(
            outcome.version == fixture.contents.version(),
            "SDK outcome changed its candidate version",
        )?;
        for (package, checksum) in SdkPackage::ALL.iter().zip(&outcome.checksums) {
            require(
                checksum == fixture.contents.checksum(*package),
                "SDK outcome changed its immutable archive checksum",
            )?;
        }
        require(
            fixture.phases == SdkConsumerPhase::ALL,
            "SDK outcome skipped or repeated a native consumer phase",
        )?;
        require(
            fixture.finishes == 1 && !execution.exists(),
            "SDK outcome preceded complete owned cleanup",
        )?;
        require(
            fs::read(artifacts.join("SOURCE.sha256"))?
                == format!("{}\n", source.source_digest()).as_bytes(),
            "SDK verification lost its native source artifact",
        )?;
        for scope in SdkConsumerScope::ALL {
            require(
                artifacts
                    .join("consumers")
                    .join(scope.label())
                    .join("Cargo.lock")
                    .is_file(),
                "SDK verification lost a checked consumer lockfile",
            )?;
        }
        Ok(())
    })
}

#[test]
fn changed_or_missing_release_projections_never_start_sdk_commands() -> io::Result<()> {
    for (path, expected) in [
        ("config/http.conf", "differs"),
        ("xtask/src/unbound.rs", "missing"),
    ] {
        using_fixture(|captured, fixture| {
            captured_fixture::write(
                &fixture.candidate.join("source"),
                path,
                b"different candidate input\n",
            )?;
            update_source_checksum(&fixture.candidate)?;
            SdkContents::read(&fixture.candidate).map_err(io::Error::other)?;
            let error = failure(captured, fixture)?;
            require(
                fixture.phases.is_empty(),
                "SDK commands ran before source binding",
            )?;
            require(
                !fixture.artifacts.exists(),
                "SDK source rejection created consumer artifacts",
            )?;
            require(
                error.contains(expected),
                "SDK source rejection lost the projection mismatch",
            )
        })?;
    }
    Ok(())
}

#[test]
fn invalid_archive_bytes_never_start_sdk_commands() -> io::Result<()> {
    using_fixture(|captured, fixture| {
        fs::write(
            fixture.candidate.join("rusterisk-api-0.1.0.crate"),
            b"invalid archive\n",
        )?;
        let error = failure(captured, fixture)?;
        require(
            fixture.phases.is_empty(),
            "SDK commands ran before archive validation",
        )?;
        require(
            !fixture.artifacts.exists(),
            "SDK archive rejection created consumer artifacts",
        )?;
        require(
            error.contains("checksum"),
            "SDK loader lost its archive checksum rejection",
        )
    })
}

#[test]
fn every_failed_native_phase_prevents_an_sdk_outcome_and_preserves_diagnostics() -> io::Result<()> {
    for phase in SdkConsumerPhase::ALL {
        using_fixture(|captured, fixture| {
            fixture.fault = Fault::Phase(phase);
            let error = failure(captured, fixture)?;
            require(
                fixture.phases.last() == Some(&phase),
                "SDK verification continued after a failed phase",
            )?;
            require(
                error.contains("fixture SDK phase failure"),
                "SDK verification lost its primary process failure",
            )?;
            require(
                fs::read(
                    fixture
                        .artifacts
                        .join(format!("{}.stderr.txt", phase.label())),
                )? == b"fixture SDK phase diagnostic\n",
                "SDK verification lost its native failure diagnostic",
            )
        })?;
    }
    Ok(())
}

#[test]
fn sdk_timeout_and_interruption_finish_supervision_without_returning_outcomes() -> io::Result<()> {
    for (fault, expected) in [
        (Fault::TimedOut, "deadline exceeded"),
        (Fault::Interrupted, "interrupted"),
    ] {
        using_fixture(|captured, fixture| {
            fixture.fault = fault;
            let error = failure(captured, fixture)?;
            require(
                error.contains(expected),
                "SDK verification lost its timeout or interruption",
            )
        })?;
    }
    Ok(())
}

#[test]
fn successful_process_ports_cannot_bypass_native_lock_metadata_or_completion_checks(
) -> io::Result<()> {
    for fault in [
        Fault::WrongLock,
        Fault::CheckoutMetadata,
        Fault::ForeignPlanMetadata,
        Fault::DetachedGraph,
        Fault::BackendDependency,
        Fault::WrongCompletion,
    ] {
        using_fixture(|captured, fixture| {
            fixture.fault = fault;
            failure(captured, fixture)?;
            require(
                fixture.phases.len() < SdkConsumerPhase::ALL.len(),
                "SDK verification continued after rejecting native output",
            )
        })?;
    }
    Ok(())
}

#[test]
fn supervisor_failure_and_source_changes_during_finish_reject_complete_sdk_consumers(
) -> io::Result<()> {
    for (fault, expected) in [
        (Fault::Finish, "fixture SDK supervisor failure"),
        (Fault::ChangedDuringFinish, "source"),
    ] {
        using_fixture(|captured, fixture| {
            fixture.fault = fault;
            let error = failure(captured, fixture)?;
            require(
                fixture.phases == SdkConsumerPhase::ALL,
                "SDK finish fault did not follow all native phases",
            )?;
            require(
                error.contains(expected),
                "SDK finish rejection lost its failed contract",
            )
        })?;
    }
    Ok(())
}

#[test]
fn phase_and_supervisor_failures_keep_both_causes() -> io::Result<()> {
    using_fixture(|captured, fixture| {
        fixture.fault = Fault::PhaseAndFinish;
        let error = failure(captured, fixture)?;
        require(
            error.contains("fixture SDK phase failure")
                && error.contains("fixture SDK supervisor failure"),
            "SDK verification discarded a primary or supervisor cleanup failure",
        )
    })
}

#[test]
fn preservation_failures_reject_sdk_outcomes_and_keep_prior_phase_errors() -> io::Result<()> {
    for fault in [Fault::Preserve, Fault::PhaseAndPreserve] {
        using_fixture(|captured, fixture| {
            fixture.fault = fault;
            let error = failure(captured, fixture)?;
            require(
                error.contains("preserv"),
                "SDK verification lost its artifact preservation failure",
            )?;
            if fault == Fault::PhaseAndPreserve {
                require(
                    error.contains("fixture SDK phase failure"),
                    "SDK preservation failure replaced its primary error",
                )?;
            }
            Ok(())
        })?;
    }
    Ok(())
}

#[test]
fn working_copy_cleanup_failures_reject_sdk_outcomes_and_keep_prior_phase_errors() -> io::Result<()>
{
    for fault in [Fault::Cleanup, Fault::PhaseAndCleanup] {
        using_fixture(|captured, fixture| {
            fixture.fault = fault;
            let error = failure(captured, fixture)?;
            require(
                error.contains("cleanup"),
                "SDK verification lost its working copy cleanup failure",
            )?;
            if fault == Fault::PhaseAndCleanup {
                require(
                    error.contains("fixture SDK phase failure"),
                    "SDK working copy cleanup failure replaced its primary error",
                )?;
            }
            Ok(())
        })?;
    }
    Ok(())
}
