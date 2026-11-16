use super::identity::EngineImage;
use super::*;
use std::{io, path::Path, process::Command};

mod fixture;

#[test]
fn docker_fixture() -> io::Result<()> {
    let Some(root) = std::env::var_os("RUSTERISK_OCI_FIXTURE_ROOT") else {
        return Ok(());
    };
    fixture::execute(Path::new(&root))
}

#[test]
fn a_runtime_must_check_the_loaded_identity_before_starting_a_container() -> io::Result<()> {
    let image = format!("sha256:{}", "a".repeat(64));
    let manifest = format!("sha256:{}", "b".repeat(64));
    let expected = ExpectedImage {
        configuration: &image,
        manifest: &manifest,
        manifest_size: 1014,
        target: ServerTarget::LinuxArm64,
    };
    let config = EngineImage::loaded(format!("Loaded image ID: {image}\n").as_bytes(), expected)?;
    assert!(matches!(config, EngineImage::Configuration(_)));
    let loaded = EngineImage::loaded(
        format!("Loaded image ID: {manifest}\n").as_bytes(),
        expected,
    )?;
    assert!(matches!(loaded, EngineImage::ApplicationManifest(_)));
    for invalid in [
        format!("Loaded image ID: sha256:{}\n", "c".repeat(64)),
        format!("Loaded image ID: {image}\nLoaded image ID: {manifest}\n"),
        "Loaded image: rusterisk:latest\n".into(),
        format!("Loaded image ID: {image}"),
        format!("Loaded image ID: {image}\n\n"),
        String::new(),
    ] {
        assert!(EngineImage::loaded(invalid.as_bytes(), expected).is_err());
    }
    Ok(())
}

#[test]
fn copying_an_archive_rejects_nonregular_empty_and_oversized_input() -> io::Result<()> {
    let root = using_root("copy")?;
    let operation = (|| {
        assert!(copy_archive(&root, &root.join("directory"), 32).is_err());
        let source = root.join("source");
        std::fs::write(&source, [])?;
        assert!(copy_archive(&source, &root.join("empty"), 32).is_err());
        std::fs::write(&source, [1; 33])?;
        assert!(copy_archive(&source, &root.join("large"), 32).is_err());
        std::fs::write(&source, [1; 32])?;
        let copy = root.join("copy");
        copy_archive(&source, &copy, 32)?;
        assert_eq!(std::fs::read(&copy)?, [1; 32]);
        assert!(copy_archive(&source, &copy, 32).is_err());
        let link = root.join("link");
        std::os::unix::fs::symlink(&source, &link)?;
        assert!(copy_archive(&link, &root.join("linked"), 32).is_err());
        Ok(())
    })();
    combine(operation, std::fs::remove_dir_all(root), "fixture cleanup")
}

fn using_root(name: &str) -> io::Result<std::path::PathBuf> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    let root = std::fs::canonicalize(std::env::temp_dir())?.join(format!(
        "rusterisk-oci-{name}-{}-{stamp}",
        std::process::id()
    ));
    std::fs::create_dir(&root)?;
    Ok(root)
}

fn runtime_case(case: &str) -> io::Result<()> {
    let root = using_root(case)?;
    let mut supervisor = CommandSupervisor::new().map_err(io::Error::other)?;
    let operation = (|| {
        let artifacts = root.join("artifacts");
        std::fs::create_dir(&artifacts)?;
        let image = format!("sha256:{}", "a".repeat(64));
        let manifest = format!("sha256:{}", "b".repeat(64));
        let archive = root.join("input.oci.tar");
        std::fs::write(&archive, b"owned archive fixture")?;
        let http_reservation = std::net::TcpListener::bind("127.0.0.1:0")?;
        let sip_reservation = std::net::UdpSocket::bind("127.0.0.1:0")?;
        let http = http_reservation.local_addr()?;
        let sip = sip_reservation.local_addr()?;
        let configuration = root.join("config");
        std::fs::create_dir(&configuration)?;
        std::fs::write(configuration.join("http"), http.to_string())?;
        std::fs::write(configuration.join("sip"), sip.to_string())?;
        let plan = ServerOciRuntimePlan::new("fixture", &image, &configuration)?;
        if case == "existing" {
            fixture::state(
                &root,
                &plan.container(ServerOciRunPhase::Serve),
                "foreign",
                &image,
                None,
            )?;
        }
        let mut client_calls = 0;
        let runtime = Runtime {
            identity: "fixture",
            configuration: &configuration,
            expected: ExpectedImage {
                configuration: &image,
                manifest: &manifest,
                manifest_size: 1014,
                target: ServerTarget::LinuxArm64,
            },
            archive: &archive,
            version: "0.1.0",
            artifacts: &artifacts,
            http_reservation: Some(http_reservation),
            sip_reservation: Some(sip_reservation),
        };
        let result = runtime_using(
            runtime,
            &mut supervisor,
            |arguments, stdout, stderr| {
                output(stdout)?;
                output(stderr)?;
                let mut command = Command::new(std::env::current_exe()?);
                command
                    .args([
                        "--exact",
                        "server_distribution::oci_sdk::tests::docker_fixture",
                        "--nocapture",
                    ])
                    .env("RUSTERISK_OCI_FIXTURE_ROOT", &root)
                    .env("RUSTERISK_OCI_FIXTURE_CASE", case)
                    .env(
                        "RUSTERISK_OCI_FIXTURE_ARGS",
                        serde_json::to_string(arguments).map_err(io::Error::other)?,
                    )
                    .env("RUSTERISK_OCI_FIXTURE_STDOUT", stdout)
                    .env("RUSTERISK_OCI_FIXTURE_STDERR", stderr)
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::inherit());
                Ok(command)
            },
            |service, address| {
                client_calls += 1;
                service.poll().map_err(io::Error::other)?;
                fixture::request(address, b'P')?;
                if matches!(case, "client-failure" | "combined-failure") {
                    Err(io::Error::other("requested client failure"))
                } else {
                    Ok(())
                }
            },
        );
        let trace = std::fs::read_to_string(root.join("trace"))?;
        match case {
            "success" | "manifest-store" => {
                result?;
                assert_eq!(client_calls, 1);
                assert_eq!(trace.lines().filter(|line| *line == "serve").count(), 2);
                assert_eq!(trace.lines().filter(|line| *line == "stop").count(), 2);
                assert!(!trace.lines().any(|line| line == "stop-by-name"));
            }
            "existing" => {
                assert!(result.is_err());
                assert_eq!(client_calls, 0);
                assert!(!trace.lines().any(|line| line == "load" || line == "remove"));
                assert!(root
                    .join("rusterisk-server-oci-check-fixture-serve.json")
                    .exists());
            }
            "changed-image" | "index-store" | "tag-load" | "multi-load" => {
                assert!(result.is_err());
                assert_eq!(client_calls, 0);
                assert!(!trace
                    .lines()
                    .any(|line| line == "serve" || line == "remove"));
                assert_eq!(
                    trace.lines().filter(|line| *line == "image").count(),
                    usize::from(case == "changed-image")
                );
            }
            "missing-ready" | "config-output" => {
                assert!(result.is_err());
                assert_eq!(client_calls, 0);
                assert_eq!(
                    trace.lines().filter(|line| *line == "stop").count(),
                    usize::from(case == "missing-ready")
                );
            }
            "client-failure" | "stop-failure" | "foreign-cleanup" | "combined-failure" => {
                let error = result
                    .err()
                    .ok_or_else(|| io::Error::other("failed runtime passed"))?;
                if matches!(case, "client-failure" | "combined-failure") {
                    assert!(
                        error.to_string().contains("requested client failure"),
                        "{case}: {error}"
                    );
                }
                if matches!(case, "foreign-cleanup" | "combined-failure") {
                    assert!(error.to_string().contains("ownership"), "{error}");
                    assert!(!trace.lines().any(|line| line == "remove"));
                }
                assert_eq!(client_calls, 1);
                if matches!(case, "client-failure" | "stop-failure") {
                    assert!(!root
                        .join("rusterisk-server-oci-check-fixture-serve.json")
                        .exists());
                }
            }
            "replacement" => {
                assert!(result.is_err());
                assert_eq!(client_calls, 1);
                assert!(root
                    .join("rusterisk-server-oci-check-fixture-serve.json")
                    .exists());
                assert!(!trace.lines().any(|line| line == "remove-by-name"));
                assert!(trace.lines().any(|line| line == "remove"));
            }
            _ => return Err(io::Error::other("unknown runtime fixture case")),
        }
        let _http = std::net::TcpListener::bind(http).map_err(|error| {
            io::Error::other(format!("{case} HTTP fixture rebind at {http}: {error}"))
        })?;
        let _sip = std::net::UdpSocket::bind(sip)
            .map_err(|error| io::Error::other(format!("{case} SIP fixture rebind: {error}")))?;
        Ok(())
    })();
    let operation = combine(
        operation,
        supervisor.finish().map_err(io::Error::other),
        "supervisor cleanup",
    );
    combine(operation, std::fs::remove_dir_all(root), "fixture cleanup")
}

#[test]
fn an_owned_runtime_loads_checks_calls_stops_and_restarts_on_the_same_ports() -> io::Result<()> {
    isolated_case("success")
}

#[test]
fn a_manifest_store_runs_only_the_loaded_verified_application_manifest() -> io::Result<()> {
    isolated_case("manifest-store")
}

#[test]
fn existing_names_and_changed_loaded_images_are_never_started_or_removed() -> io::Result<()> {
    for case in [
        "existing",
        "changed-image",
        "index-store",
        "tag-load",
        "multi-load",
    ] {
        isolated_case(case)?;
    }
    Ok(())
}

#[test]
fn failed_clients_and_stop_requests_cleanup_only_owned_containers() -> io::Result<()> {
    for case in [
        "client-failure",
        "stop-failure",
        "foreign-cleanup",
        "combined-failure",
        "replacement",
    ] {
        isolated_case(case)?;
    }
    Ok(())
}

#[test]
fn invalid_configuration_output_and_missing_readiness_never_run_the_client() -> io::Result<()> {
    for case in ["config-output", "missing-ready"] {
        isolated_case(case)?;
    }
    Ok(())
}

#[test]
fn runtime_owner_fixture() -> io::Result<()> {
    let Some(case) = std::env::var_os("RUSTERISK_OCI_RUNTIME_OWNER") else {
        return Ok(());
    };
    let driver = std::env::var("RUSTERISK_OCI_RUNTIME_DRIVER").map_err(io::Error::other)?;
    assert_ne!(driver, std::process::id().to_string());
    let case = case
        .to_str()
        .ok_or_else(|| io::Error::other("invalid runtime fixture case"))?;
    runtime_case(case)
}

fn isolated_case(case: &str) -> io::Result<()> {
    let root = using_root("owner")?;
    let mut supervisor = CommandSupervisor::new().map_err(io::Error::other)?;
    let operation = (|| {
        let stdout = root.join("stdout");
        let stderr = root.join("stderr");
        let mut command = Command::new(std::env::current_exe()?);
        command
            .args([
                "--exact",
                "server_distribution::oci_sdk::tests::runtime_owner_fixture",
                "--nocapture",
            ])
            .env("RUSTERISK_OCI_RUNTIME_OWNER", case)
            .env(
                "RUSTERISK_OCI_RUNTIME_DRIVER",
                std::process::id().to_string(),
            )
            .stdout(output(&stdout)?)
            .stderr(output(&stderr)?);
        let operation = supervisor.run(&mut command, std::time::Duration::from_secs(30));
        let out = crate::server_distribution::native::read(&stdout, 8192)?;
        let err = crate::server_distribution::native::read(&stderr, 8192)?;
        operation.map_err(|error| {
            io::Error::other(format!(
                "isolated {case} fixture failed: {error}; stdout={}; stderr={}",
                String::from_utf8_lossy(&out),
                String::from_utf8_lossy(&err)
            ))
        })
    })();
    let operation = combine(
        operation,
        supervisor.finish().map_err(io::Error::other),
        "fixture supervisor cleanup",
    );
    combine(
        operation,
        std::fs::remove_dir_all(root),
        "fixture owner cleanup",
    )
}

#[test]
fn candidate_bytes_are_required_before_creating_runtime_artifacts() -> io::Result<()> {
    let root = using_root("missing-candidate")?;
    let mut supervisor = CommandSupervisor::new().map_err(io::Error::other)?;
    let operation = {
        let missing = root.join("missing");
        assert!(verify_server_oci_sdk_with(
            &root,
            &missing,
            crate::server_distribution::ServerTarget::LinuxArm64,
            &missing,
            &missing,
            &mut supervisor,
        )
        .is_err());
        assert!(!root.join(".cache").exists());
        Ok(())
    };
    let operation = combine(
        operation,
        supervisor.finish().map_err(io::Error::other),
        "supervisor cleanup",
    );
    combine(operation, std::fs::remove_dir_all(root), "fixture cleanup")
}
