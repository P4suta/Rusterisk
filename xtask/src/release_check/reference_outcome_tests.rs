use super::{verify_reference_with, ReferencePort};
use crate::{
    process::ProcessError,
    reference::{Interop, ReferenceImage, ReferenceRunner},
    release_check::{CapturedReleaseSource, SourceInputs},
};
use std::{
    fs, io,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Copy)]
enum Fault {
    None,
    MissingInspection,
    InvalidInspection,
    ForeignImage,
    InspectionFailure,
    MissingInput,
    ComparisonFailure,
    TimedOut,
    Interrupted,
    FinishFailure,
    ComparisonAndFinishFailure,
    ChangedDuringComparison,
    ChangedDuringFinish,
    MissingWorkspaceAtFinish,
    ReplacedWorkspaceAtFinish,
}

struct Fixture {
    working: PathBuf,
    fault: Fault,
    inspections: usize,
    comparisons: usize,
    finishes: usize,
    late_mutations: usize,
}

struct Port<'a> {
    fixture: &'a mut Fixture,
    captured: &'a CapturedReleaseSource,
    image: &'a ReferenceImage,
}

fn require(valid: bool, reason: &'static str) -> io::Result<()> {
    if valid {
        Ok(())
    } else {
        Err(io::Error::other(reason))
    }
}

fn configuration_path(source: &Path, identity: &str) -> io::Result<PathBuf> {
    ReferenceRunner::configuration_path(source, identity).map_err(io::Error::other)
}

fn checked_configuration(source: &Path) -> io::Result<()> {
    let configuration = configuration_path(source, "fixture")?;
    let parent = source.join("target/interop");
    require(
        configuration.parent() == Some(parent.as_path())
            && configuration.file_name() == Some(std::ffi::OsStr::new("fixture.configuration")),
        "Reference configuration escaped the captured source transfer guard",
    )?;
    let configuration = configuration
        .to_str()
        .ok_or_else(|| io::Error::other("Reference configuration path is not UTF-8"))?;
    let plan = Interop::new("fixture", configuration).map_err(io::Error::other)?;
    let start = plan.start();
    let expected = format!("type=bind,source={configuration},target=/etc/asterisk,readonly");
    require(
        start
            .arguments
            .windows(2)
            .filter(|pair| pair[0] == "--mount" && pair[1] == expected)
            .count()
            == 1,
        "Reference container mount differs from the checked transfer configuration",
    )?;
    for invalid in ["", "../foreign", "-foreign"] {
        require(
            configuration_path(source, invalid).is_err(),
            "Invalid execution identity selected a reference configuration",
        )?;
    }
    require(
        configuration_path(Path::new("relative/source"), "fixture").is_err(),
        "Relative source selected a reference configuration",
    )
}

impl Port<'_> {
    fn check_request(&self, source: &Path, image: &ReferenceImage) -> io::Result<()> {
        require(
            source.canonicalize()? == self.fixture.working.join("source").canonicalize()?,
            "Reference execution escaped its owned source copy",
        )?;
        self.captured.check(source).map_err(io::Error::other)?;
        require(
            image.as_str() == self.image.as_str(),
            "Reference execution changed its selected image",
        )
    }
}

impl ReferencePort for Port<'_> {
    fn inspect_image(
        &mut self,
        source: &Path,
        image: &ReferenceImage,
    ) -> Result<String, ProcessError> {
        self.check_request(source, image)?;
        self.fixture.inspections += 1;
        require(
            self.fixture.inspections == 1 && self.fixture.comparisons == 0,
            "Reference image inspection was skipped, repeated, or late",
        )?;
        match self.fixture.fault {
            Fault::MissingInspection => Ok(String::new()),
            Fault::InvalidInspection => Ok("rusterisk-asterisk-reference:22.9.0\n".into()),
            Fault::ForeignImage => Ok(format!("sha256:{}\n", "b".repeat(64))),
            Fault::InspectionFailure => Err(ProcessError::InvalidOutput),
            Fault::None
            | Fault::MissingInput
            | Fault::ComparisonFailure
            | Fault::TimedOut
            | Fault::Interrupted
            | Fault::FinishFailure
            | Fault::ComparisonAndFinishFailure
            | Fault::ChangedDuringComparison
            | Fault::ChangedDuringFinish
            | Fault::MissingWorkspaceAtFinish
            | Fault::ReplacedWorkspaceAtFinish => Ok(format!("{}\n", image.as_str())),
        }
    }

    fn compare(&mut self, source: &Path, image: &ReferenceImage) -> Result<(), ProcessError> {
        self.check_request(source, image)?;
        checked_configuration(source)?;
        self.fixture.comparisons += 1;
        require(
            self.fixture.inspections == 1 && self.fixture.comparisons == 1,
            "Reference comparisons ran without a single preceding image inspection",
        )?;
        match self.fixture.fault {
            Fault::MissingInput => {
                fs::remove_file(source.join("tests/interop/sipp/registration.xml"))?;
            }
            Fault::ChangedDuringComparison => {
                fs::write(source.join("Cargo.lock"), b"different reference source")?;
            }
            Fault::ComparisonFailure => return Err(ProcessError::InvalidOutput),
            Fault::TimedOut | Fault::ComparisonAndFinishFailure => {
                return Err(ProcessError::Timeout)
            }
            Fault::Interrupted => return Err(ProcessError::Interrupted),
            Fault::None
            | Fault::MissingInspection
            | Fault::InvalidInspection
            | Fault::ForeignImage
            | Fault::InspectionFailure
            | Fault::FinishFailure
            | Fault::ChangedDuringFinish
            | Fault::MissingWorkspaceAtFinish
            | Fault::ReplacedWorkspaceAtFinish => {}
        }
        Ok(())
    }

    fn finish(self) -> Result<(), ProcessError> {
        self.fixture.finishes += 1;
        match self.fixture.fault {
            Fault::FinishFailure | Fault::ComparisonAndFinishFailure => {
                Err(ProcessError::InvalidOutput)
            }
            Fault::ChangedDuringFinish => {
                fs::write(
                    self.fixture.working.join("source/Cargo.lock"),
                    b"source changed before supervised descendants finished",
                )?;
                self.fixture.late_mutations += 1;
                Ok(())
            }
            Fault::MissingWorkspaceAtFinish => {
                fs::remove_dir_all(&self.fixture.working)?;
                Ok(())
            }
            Fault::ReplacedWorkspaceAtFinish => {
                fs::remove_dir_all(&self.fixture.working)?;
                fs::write(&self.fixture.working, b"replaced owned workspace")?;
                Ok(())
            }
            Fault::None
            | Fault::MissingInspection
            | Fault::InvalidInspection
            | Fault::ForeignImage
            | Fault::InspectionFailure
            | Fault::MissingInput
            | Fault::ComparisonFailure
            | Fault::TimedOut
            | Fault::Interrupted
            | Fault::ChangedDuringComparison => Ok(()),
        }
    }
}

fn using_fixture(
    test: impl FnOnce(&CapturedReleaseSource, &ReferenceImage, &mut Fixture) -> io::Result<()>,
) -> io::Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or_else(|| io::Error::other("workspace missing"))?;
    let captured = CapturedReleaseSource::read(root).map_err(io::Error::other)?;
    let image = ReferenceImage::parse_inspection(&format!("sha256:{}\n", "a".repeat(64)))
        .map_err(io::Error::other)?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    let parent = std::env::temp_dir().join(format!(
        "rusterisk-reference-outcome-tests-{}-{stamp}",
        std::process::id()
    ));
    fs::create_dir(&parent)?;
    let mut fixture = Fixture {
        working: parent.join("execution"),
        fault: Fault::None,
        inspections: 0,
        comparisons: 0,
        finishes: 0,
        late_mutations: 0,
    };
    let operation = test(&captured, &image, &mut fixture);
    match (operation, fs::remove_dir_all(parent)) {
        (operation, Ok(())) => operation,
        (Ok(()), Err(error)) => Err(error),
        (Err(operation), Err(cleanup)) => Err(io::Error::other(format!(
            "{operation}; reference outcome fixture cleanup failed: {cleanup}"
        ))),
    }
}

#[test]
fn reference_outcomes_bind_inputs_and_image_after_comparison_and_owned_cleanup() -> io::Result<()> {
    using_fixture(|captured, image, fixture| {
        let expected = captured.inputs().map_err(io::Error::other)?.key();
        let working = fixture.working.clone();
        let outcome = verify_reference_with(
            captured,
            image,
            &working,
            Port {
                fixture,
                captured,
                image,
            },
        )
        .map_err(io::Error::other)?;
        require(
            SourceInputs::new(outcome.inputs())
                .map_err(io::Error::other)?
                .key()
                == expected,
            "Reference outcome was bound to another source",
        )?;
        require(
            outcome.image().as_str() == image.as_str(),
            "Reference outcome was bound to another image",
        )?;
        require(
            fixture.inspections == 1 && fixture.comparisons == 1 && fixture.finishes == 1,
            "Reference verification skipped or repeated an execution stage",
        )?;
        require(
            !working.exists(),
            "Reference outcome retained its owned state",
        )
    })
}

#[test]
fn missing_mutable_or_foreign_image_inspections_cannot_admit_comparisons() -> io::Result<()> {
    for fault in [
        Fault::MissingInspection,
        Fault::InvalidInspection,
        Fault::ForeignImage,
        Fault::InspectionFailure,
    ] {
        using_fixture(|captured, image, fixture| {
            fixture.fault = fault;
            let working = fixture.working.clone();
            let outcome = verify_reference_with(
                captured,
                image,
                &working,
                Port {
                    fixture,
                    captured,
                    image,
                },
            );
            require(outcome.is_err(), "An invalid image produced an outcome")?;
            require(
                fixture.inspections == 1 && fixture.comparisons == 0 && fixture.finishes == 1,
                "Invalid image inspection ran comparisons or skipped cleanup",
            )?;
            require(
                !working.exists(),
                "Image rejection retained its owned state",
            )
        })?;
    }
    Ok(())
}

#[test]
fn failed_changed_or_unfinished_reference_work_never_constructs_an_outcome() -> io::Result<()> {
    for fault in [
        Fault::MissingInput,
        Fault::ComparisonFailure,
        Fault::TimedOut,
        Fault::Interrupted,
        Fault::FinishFailure,
        Fault::ChangedDuringComparison,
        Fault::ChangedDuringFinish,
        Fault::MissingWorkspaceAtFinish,
        Fault::ReplacedWorkspaceAtFinish,
    ] {
        using_fixture(|captured, image, fixture| {
            fixture.fault = fault;
            let working = fixture.working.clone();
            let outcome = verify_reference_with(
                captured,
                image,
                &working,
                Port {
                    fixture,
                    captured,
                    image,
                },
            );
            require(
                outcome.is_err(),
                "Incomplete reference work produced an outcome",
            )?;
            if matches!(
                fault,
                Fault::MissingWorkspaceAtFinish | Fault::ReplacedWorkspaceAtFinish
            ) {
                let error = outcome
                    .err()
                    .ok_or_else(|| io::Error::other("Missing workspace produced an outcome"))?
                    .to_string();
                require(
                    error.contains("cleanup"),
                    "Owned workspace cleanup failure was lost",
                )?;
            }
            require(
                fixture.finishes == 1,
                "Reference cleanup did not finish once",
            )?;
            if matches!(fault, Fault::ChangedDuringFinish) {
                require(
                    fixture.late_mutations == 1,
                    "Source state was removed before supervised work finished",
                )?;
            }
            if !matches!(fault, Fault::ReplacedWorkspaceAtFinish) {
                require(
                    !working.exists(),
                    "Failed reference work retained owned state",
                )?;
            }
            Ok(())
        })?;
    }
    Ok(())
}

#[test]
fn comparison_and_supervisor_failures_both_survive_reference_cleanup() -> io::Result<()> {
    using_fixture(|captured, image, fixture| {
        fixture.fault = Fault::ComparisonAndFinishFailure;
        let working = fixture.working.clone();
        let error = verify_reference_with(
            captured,
            image,
            &working,
            Port {
                fixture,
                captured,
                image,
            },
        )
        .err()
        .ok_or_else(|| io::Error::other("Failed reference work produced an outcome"))?
        .to_string();
        require(
            error.contains("deadline exceeded"),
            "Comparison failure was lost",
        )?;
        require(
            error.contains("invalid bounded process output"),
            "Supervisor failure was lost",
        )?;
        require(fixture.finishes == 1, "Combined failure skipped cleanup")?;
        require(!working.exists(), "Combined failure retained owned state")
    })
}

#[test]
fn captured_reference_inputs_supply_the_complete_maintained_comparison_inventory() -> io::Result<()>
{
    using_fixture(|captured, _image, _fixture| {
        for path in [
            "justfile",
            "mise.toml",
            ".cargo/config.toml",
            "Cargo.toml",
            "Cargo.lock",
            "xtask/src/interop.rs",
            "xtask/src/main.rs",
            "xtask/src/reference.rs",
            "xtask/src/reference/sipp_comparison.rs",
            "tests/interop/asterisk/http.conf",
            "tests/interop/asterisk/ari.conf",
            "tests/interop/asterisk/pjsip.conf",
            "tests/interop/asterisk/rtp.conf",
            "tests/interop/asterisk/extensions.conf",
            "tests/interop/sipp/registration.xml",
            "tests/interop/node-ari-client/package.json",
            "tests/interop/node-ari-client/package-lock.json",
            "crates/client/tests/reference.rs",
            "crates/server/tests/sip_call.rs",
            "crates/server/tests/sip_transfer/blind.rs",
            "crates/server/tests/sip_transfer/attended.rs",
        ] {
            require(
                captured
                    .files()
                    .any(|(actual, bytes)| actual == Path::new(path) && !bytes.is_empty()),
                "A maintained reference comparison has no captured input",
            )?;
        }
        Ok(())
    })
}
