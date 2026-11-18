use super::{
    super::{
        retained::{comparison_profile, comparison_results, execute_with, read, BrowserPort},
        BrowserCommand, PreparedBrowserRun, RunError,
    },
    fixture::{using_fixture, Fault, Fixture},
};
use crate::{browser::SourceSnapshot, process::ProcessError};
use std::{io, process::Command, time::Duration};

#[test]
fn retained_browser_plan_captures_the_package_managers_used_by_native_phases() -> io::Result<()> {
    using_fixture(|fixture| {
        let prepared =
            PreparedBrowserRun::read(&fixture.root, &fixture.image).map_err(io::Error::other)?;
        for (phase, expected) in [
            (crate::browser::Phase::CargoVersion, ["cargo", "-vV"]),
            (crate::browser::Phase::NpmVersion, ["npm", "--version"]),
        ] {
            let step = prepared.plan().step(phase);
            let tail: Vec<_> = step
                .arguments
                .iter()
                .rev()
                .take(2)
                .rev()
                .map(String::as_str)
                .collect();
            assert_eq!(tail, expected);
            assert!(step.budget.as_secs() <= 30);
        }
        Ok(())
    })
}

struct Port<'a> {
    fixture: &'a mut Fixture,
    finished: &'a mut usize,
    fail_finish: bool,
}

impl BrowserPort for Port<'_> {
    fn run(
        &mut self,
        command: &mut Command,
        budget: Duration,
        mode: BrowserCommand,
    ) -> Result<(), ProcessError> {
        self.fixture.perform(command, budget, mode)?;
        if mode == BrowserCommand::Run
            && self.fixture.phases.last() == Some(&crate::browser::Phase::Run)
        {
            self.fixture.write_retained_runtime()?;
        }
        Ok(())
    }

    fn finish(self) -> Result<(), ProcessError> {
        *self.finished += 1;
        if self.fail_finish {
            Err(ProcessError::Interrupted)
        } else {
            Ok(())
        }
    }
}

#[test]
fn retained_browser_lifecycle_keeps_inventory_run_audio_and_original_execution_profile(
) -> io::Result<()> {
    using_fixture(|fixture| {
        let prepared =
            PreparedBrowserRun::read(&fixture.root, &fixture.image).map_err(io::Error::other)?;
        let destination = fixture.root.join("retained");
        let mut finished = 0;
        let retained = execute_with(
            prepared,
            &destination,
            Port {
                fixture,
                finished: &mut finished,
                fail_finish: false,
            },
        )
        .map_err(io::Error::other)?;
        assert_eq!(finished, 1);
        assert!(!fixture.working()?.exists());
        assert!(destination.join("browser-inventory.json").is_file());
        assert!(destination.join("browser-results.json").is_file());
        let snapshot = SourceSnapshot::read(&fixture.root).map_err(io::Error::other)?;
        let checked = read(snapshot, &fixture.image, &destination, retained.execution())
            .map_err(io::Error::other)?;
        assert_eq!(checked.inventory().count(), 4);
        assert_eq!(checked.reference_image(), None);
        assert_eq!(checked.artifacts(), destination);
        assert_ne!(
            std::fs::read(destination.join("browser-inventory.json"))?,
            std::fs::read(destination.join("browser-results.json"))?
        );
        Ok(())
    })
}

#[test]
fn retained_browser_supervisor_failure_cannot_preserve_reusable_native_results() -> io::Result<()> {
    using_fixture(|fixture| {
        let prepared =
            PreparedBrowserRun::read(&fixture.root, &fixture.image).map_err(io::Error::other)?;
        let destination = fixture.root.join("retained");
        let mut finished = 0;
        let result = execute_with(
            prepared,
            &destination,
            Port {
                fixture,
                finished: &mut finished,
                fail_finish: true,
            },
        );
        assert!(matches!(
            result,
            Err(RunError::Process(ProcessError::Interrupted))
        ));
        assert_eq!(finished, 1);
        assert!(!destination.exists());
        assert!(!fixture.working()?.exists());
        Ok(())
    })
}

#[test]
fn retained_browser_changed_source_cannot_preserve_reusable_native_results() -> io::Result<()> {
    using_fixture(|fixture| {
        fixture.fault = Fault::ChangedSource;
        let prepared =
            PreparedBrowserRun::read(&fixture.root, &fixture.image).map_err(io::Error::other)?;
        let destination = fixture.root.join("retained");
        let mut finished = 0;
        let result = execute_with(
            prepared,
            &destination,
            Port {
                fixture,
                finished: &mut finished,
                fail_finish: false,
            },
        );
        assert!(result.is_err());
        assert_eq!(finished, 1);
        assert!(!destination.exists());
        assert!(!fixture.working()?.exists());
        Ok(())
    })
}

#[test]
fn paired_retained_profile_binds_both_images_to_the_captured_source() -> io::Result<()> {
    let source = "c".repeat(64);
    let image = format!("sha256:{}", "a".repeat(64));
    let reference = format!("sha256:{}", "b".repeat(64));
    let expected = format!("source={source}\nbrowser={image}\nasterisk={reference}\n");
    comparison_profile(expected.as_bytes(), &source, &image, &reference)
        .map_err(io::Error::other)?;
    for changed in [
        String::new(),
        expected.replace(&source, &"d".repeat(64)),
        expected.replace(&image, &format!("sha256:{}", "e".repeat(64))),
        expected.replace(&reference, &format!("sha256:{}", "f".repeat(64))),
        expected.replace("asterisk=", "reference="),
        expected.replace('\n', "\r\n"),
        format!("{expected}extra=true\n"),
    ] {
        assert!(comparison_profile(changed.as_bytes(), &source, &image, &reference).is_err());
    }
    assert!(comparison_profile(
        expected.as_bytes(),
        &source,
        &image,
        &format!("sha256:{}", "0".repeat(64)),
    )
    .is_err());
    Ok(())
}

#[test]
fn paired_retained_results_require_the_same_inventory_and_reference_visual_audio_oracles(
) -> io::Result<()> {
    using_fixture(|fixture| {
        let prepared =
            PreparedBrowserRun::read(&fixture.root, &fixture.image).map_err(io::Error::other)?;
        let destination = fixture.root.join("retained");
        let mut finished = 0;
        let retained = execute_with(
            prepared,
            &destination,
            Port {
                fixture,
                finished: &mut finished,
                fail_finish: false,
            },
        )
        .map_err(io::Error::other)?;
        let snapshot = SourceSnapshot::read(&fixture.root).map_err(io::Error::other)?;
        let outcome = read(snapshot, &fixture.image, &destination, retained.execution())
            .map_err(io::Error::other)?;
        let reference_image = format!("sha256:{}", "b".repeat(64));
        let snapshot = SourceSnapshot::read(&fixture.root).map_err(io::Error::other)?;
        assert!(retained
            .execution()
            .read_selected(
                snapshot,
                &fixture.image,
                Some(&reference_image),
                &destination,
            )
            .is_err());
        assert!(comparison_results(&destination, outcome.inventory()).is_err());
        let report = destination.join("reference-browser-results.json");
        std::fs::copy(destination.join("browser-results.json"), &report)?;
        let reference = destination.join("reference-screenshots");
        super::super::copy_tree(
            &destination.join("screenshots"),
            &reference,
            0,
            &mut 0,
            &mut 0,
        )?;
        std::fs::write(
            destination.join("reference-run.txt"),
            b"reference browser completed\n",
        )?;
        comparison_results(&destination, outcome.inventory()).map_err(io::Error::other)?;
        let snapshot = SourceSnapshot::read(&fixture.root).map_err(io::Error::other)?;
        std::fs::write(
            destination.join("reference-images.txt"),
            format!(
                "source={}\nbrowser={}\nasterisk={reference_image}\n",
                snapshot.digest(),
                fixture.image
            ),
        )?;
        let paired = retained
            .execution()
            .read_selected(
                snapshot,
                &fixture.image,
                Some(&reference_image),
                &destination,
            )
            .map_err(io::Error::other)?;
        assert_eq!(paired.reference_image(), Some(reference_image.as_str()));
        for expected in [None, Some(format!("sha256:{}", "0".repeat(64)))] {
            let snapshot = SourceSnapshot::read(&fixture.root).map_err(io::Error::other)?;
            assert!(retained
                .execution()
                .read_selected(snapshot, &fixture.image, expected.as_deref(), &destination,)
                .is_err());
        }
        let original_report = std::fs::read(&report)?;
        let mut foreign: serde_json::Value =
            serde_json::from_slice(&original_report).map_err(io::Error::other)?;
        foreign["suites"][0]["specs"][0]["id"] = serde_json::json!("foreign-native-scenario");
        std::fs::write(
            &report,
            serde_json::to_vec(&foreign).map_err(io::Error::other)?,
        )?;
        assert!(comparison_results(&destination, outcome.inventory()).is_err());
        std::fs::write(&report, original_report)?;
        for path in [
            report,
            reference.join("browser-320.png"),
            reference.join("runtime/browser-known.wav"),
        ] {
            let original = std::fs::read(&path)?;
            std::fs::remove_file(&path)?;
            assert!(comparison_results(&destination, outcome.inventory()).is_err());
            std::fs::write(&path, b"invalid retained reference bytes")?;
            assert!(comparison_results(&destination, outcome.inventory()).is_err());
            std::fs::write(path, original)?;
        }
        let recording = reference.join("runtime/browser-known.wav");
        let original = std::fs::read(&recording)?;
        crate::browser::runtime_inputs_tests::wave(&recording, 8000, &[440.0])?;
        assert!(comparison_results(&destination, outcome.inventory()).is_err());
        std::fs::write(recording, original)?;
        let run = destination.join("reference-run.txt");
        let output = std::fs::read(&run)?;
        std::fs::remove_file(&run)?;
        assert!(comparison_results(&destination, outcome.inventory()).is_err());
        std::fs::write(run, output)?;
        let extra = reference.join("foreign.png");
        std::fs::copy(reference.join("browser-320.png"), &extra)?;
        assert!(comparison_results(&destination, outcome.inventory()).is_err());
        std::fs::remove_file(extra)?;
        comparison_results(&destination, outcome.inventory()).map_err(io::Error::other)?;
        let marker = destination.join("reference-images.txt");
        let profile = std::fs::read(&marker)?;
        std::fs::remove_file(&marker)?;
        let snapshot = SourceSnapshot::read(&fixture.root).map_err(io::Error::other)?;
        assert!(retained
            .execution()
            .read_selected(
                snapshot,
                &fixture.image,
                Some(&reference_image),
                &destination,
            )
            .is_err());
        std::fs::write(marker, profile)?;
        assert_eq!(finished, 1);
        Ok(())
    })
}
