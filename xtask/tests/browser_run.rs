use xtask::browser::{Phase, RunPlan};

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
            "--pids-limit=256",
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
