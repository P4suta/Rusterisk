use xtask::browser::{Phase, RunPlan};

#[test]
fn browser_runtime_uses_bounded_tmpfs_and_keeps_reports_on_owned_state(
) -> Result<(), xtask::browser::BrowserError> {
    let image = format!("sha256:{}", "a".repeat(64));
    let plan = RunPlan::new("fixture", "/owned/source", "/owned/state", &image)?;
    for phase in Phase::ALL {
        let arguments = plan.step(phase).arguments;
        let temporary: Vec<_> = arguments
            .iter()
            .filter(|value| value.starts_with("--env=TMPDIR="))
            .map(String::as_str)
            .collect();
        let expected = if phase == Phase::Run {
            "--env=TMPDIR=/tmp"
        } else {
            "--env=TMPDIR=/state/tmp"
        };
        assert_eq!(temporary, [expected], "{}", phase.label());
        let mounts: Vec<_> = arguments
            .windows(2)
            .filter(|pair| pair[0] == "--tmpfs")
            .map(|pair| pair[1].as_str())
            .collect();
        assert_eq!(mounts, ["/tmp:mode=1777,size=128m"]);
        for persistent in [
            "type=bind,source=/owned/state,target=/state",
            if phase == Phase::Inventory {
                "--env=PLAYWRIGHT_JSON_OUTPUT_FILE=/state/browser-inventory.json"
            } else {
                "--env=PLAYWRIGHT_JSON_OUTPUT_FILE=/state/browser-results.json"
            },
            "--env=RUSTERISK_BROWSER_ARTIFACTS=/state/browser-artifacts",
        ] {
            assert_eq!(
                arguments
                    .iter()
                    .filter(|value| *value == persistent)
                    .count(),
                1,
                "{}: {persistent}",
                phase.label()
            );
        }
    }
    let inventory = plan.step(Phase::Inventory);
    let run = plan.step(Phase::Run);
    assert!(inventory.arguments.iter().any(|argument| {
        argument == "--env=PLAYWRIGHT_JSON_OUTPUT_FILE=/state/browser-inventory.json"
    }));
    assert!(!run.arguments.iter().any(|argument| {
        argument == "--env=PLAYWRIGHT_JSON_OUTPUT_FILE=/state/browser-inventory.json"
    }));
    assert!(plan.step(Phase::Run).arguments.ends_with(
        &[
            "cargo",
            "test",
            "--locked",
            "--offline",
            "--package",
            "rusterisk-server",
            "--test",
            "browser_calls",
            "--",
            "--include-ignored",
            "--test-threads=1",
            "--nocapture",
        ]
        .map(str::to_owned)
    ));
    Ok(())
}

#[test]
fn browser_execution_bounds_every_phase_and_restricts_network_to_dependency_fetching(
) -> Result<(), xtask::browser::BrowserError> {
    let image = format!("sha256:{}", "a".repeat(64));
    let plan = RunPlan::new("fixture", "/owned/source", "/owned/state", &image)?;
    for phase in Phase::ALL {
        let step = plan.step(phase);
        for required in [
            "--rm",
            "--pull=never",
            "--cpus=2",
            "--memory=2g",
            "--memory-swap=2g",
            "--read-only",
            "--init",
            "--shm-size=512m",
            "--cap-drop=ALL",
            "--security-opt=no-new-privileges",
        ] {
            assert!(
                step.arguments.iter().any(|value| value == required),
                "{required}"
            );
        }
        let process_limit = if phase == Phase::Run {
            "--pids-limit=512"
        } else {
            "--pids-limit=256"
        };
        let limits: Vec<_> = step
            .arguments
            .iter()
            .filter(|value| value.starts_with("--pids-limit="))
            .map(String::as_str)
            .collect();
        assert_eq!(limits, [process_limit], "{}", phase.label());
        let network = if phase.downloads() {
            "--network=bridge"
        } else {
            "--network=none"
        };
        assert!(step.arguments.iter().any(|value| value == network));
        assert!(step.budget.as_secs() <= 900 && step.budget.as_secs() > 20);
        assert!(step
            .arguments
            .iter()
            .any(|value| value == "type=bind,source=/owned/source,target=/project,readonly"));
        assert!(step.arguments.iter().any(|value| value
            == "type=bind,source=/owned/state/browser,target=/project/examples/browser"));
        let cleanup = plan.cleanup(phase);
        assert_eq!(&cleanup.arguments[..2], ["rm", "--force"]);
        assert_eq!(cleanup.arguments.len(), 3);
        assert!(cleanup.arguments[2].starts_with("rusterisk-browser-fixture-"));
    }
    for image in ["latest", "x:tag", "sha256:bad", "@sha256:aaaaaaaa"] {
        assert!(RunPlan::new("fixture", "/source", "/state", image).is_err());
    }
    for identity in ["", "--all", "../shared", "with space", &"x".repeat(65)] {
        assert!(RunPlan::new(identity, "/source", "/state", &image).is_err());
    }
    for path in ["relative", "/with,comma", "/with\nnewline"] {
        assert!(RunPlan::new("fixture", path, "/state", &image).is_err());
    }
    assert!(RunPlan::new("fixture", "/same", "/same", &image).is_err());
    Ok(())
}

#[test]
fn browser_compile_admits_the_native_test_binary_without_expanding_other_file_limits(
) -> Result<(), xtask::browser::BrowserError> {
    let image = format!("sha256:{}", "a".repeat(64));
    let plan = RunPlan::new("fixture", "/owned/source", "/owned/state", &image)?;
    for phase in Phase::ALL {
        let expected = if phase == Phase::Compile {
            "fsize=536870912:536870912"
        } else {
            "fsize=134217728:134217728"
        };
        assert!(
            plan.step(phase)
                .arguments
                .iter()
                .any(|argument| argument == expected),
            "{}",
            phase.label()
        );
    }
    Ok(())
}
