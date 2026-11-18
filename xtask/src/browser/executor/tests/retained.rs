use super::{
    super::{
        retained::{execute_with, read, BrowserPort},
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
