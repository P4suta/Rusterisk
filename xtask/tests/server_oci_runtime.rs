use std::{io, path::Path, time::Duration};
use xtask::server_distribution::{ServerOciRunPhase, ServerOciRuntimePlan};

#[test]
fn image_runtime_checks_bound_resources_and_keep_configuration_read_only() -> io::Result<()> {
    let image = format!("sha256:{}", "a".repeat(64));
    let plan = ServerOciRuntimePlan::new("fixture", &image, Path::new("/runtime/config"))?;
    for phase in ServerOciRunPhase::ALL {
        let step = plan.step(phase);
        assert!(step
            .arguments
            .starts_with(&["run".to_owned(), "--rm".to_owned()]));
        assert!(step.budget <= Duration::from_secs(90));
        for argument in [
            "--pull=never",
            "--cpus=2",
            "--memory=2g",
            "--memory-swap=2g",
            "--pids-limit=128",
            "--cap-drop=ALL",
            "--security-opt=no-new-privileges",
            image.as_str(),
        ] {
            assert!(
                step.arguments.iter().any(|value| value == argument),
                "{argument}"
            );
        }
        assert!(step.arguments.windows(2).any(|pair| {
            pair[0] == "--label"
                && pair[1]
                    == format!(
                        "io.rusterisk.distribution.check=rusterisk-server-oci-check-fixture-{}",
                        phase.label()
                    )
        }));
        let mounted = step.arguments.iter().any(|value| {
            value == "type=bind,source=/runtime/config,target=/run/rusterisk-config,readonly"
        });
        assert_eq!(
            mounted,
            matches!(
                phase,
                ServerOciRunPhase::RuntimeConfig | ServerOciRunPhase::Serve
            )
        );
        let network = if matches!(phase, ServerOciRunPhase::Serve) {
            "--network=host"
        } else {
            "--network=none"
        };
        assert!(step.arguments.iter().any(|value| value == network));
        assert!(!step.arguments.iter().any(|value| value == "--privileged"
            || value == "--user"
            || value == "--entrypoint"
            || value == "--publish"));
    }
    let stop = plan.stop();
    assert_eq!(
        stop.arguments,
        [
            "stop",
            "--timeout=5",
            "rusterisk-server-oci-check-fixture-serve"
        ]
    );
    assert_eq!(stop.budget, Duration::from_secs(15));
    let cleanup = plan.cleanup(ServerOciRunPhase::Serve);
    assert_eq!(
        cleanup.arguments,
        ["rm", "--force", "rusterisk-server-oci-check-fixture-serve"]
    );
    Ok(())
}

#[test]
fn image_runtime_checks_refuse_mutable_images_and_ambiguous_mounts() {
    let image = format!("sha256:{}", "a".repeat(64));
    for (identity, image, path) in [
        ("", image.as_str(), "/runtime/config"),
        ("fixture", "rusterisk:latest", "/runtime/config"),
        ("fixture", image.as_str(), "relative"),
        ("fixture", image.as_str(), "/runtime/../config"),
        ("fixture", image.as_str(), "/runtime,other/config"),
    ] {
        assert!(ServerOciRuntimePlan::new(identity, image, Path::new(path)).is_err());
    }
}
