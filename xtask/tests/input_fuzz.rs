use xtask::input_fuzz::{
    normalize_manifest, validate_inventory, validate_run, FuzzError, FuzzPlan, LinuxTarget, Phase,
    Target,
};

const IMAGE: &str = "registry.example/verification@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

#[test]
fn fuzz_phases_separate_fetch_build_and_native_execution_with_owned_limits() -> Result<(), FuzzError>
{
    let plan = FuzzPlan::new("fixture", "/source", "/state", IMAGE, LinuxTarget::Arm64)?;
    for phase in [
        Phase::Version,
        Phase::Inventory,
        Phase::Fetch,
        Phase::Build(Target::Settings),
        Phase::Run(Target::Settings),
    ] {
        let step = plan.step(phase);
        for expected in [
            "--rm",
            "--pull=never",
            "--cpus=2",
            "--memory=2g",
            "--memory-swap=2g",
            "--pids-limit=256",
            "--read-only",
            "--cap-drop=ALL",
            "--security-opt=no-new-privileges",
            "--env=CARGO_BUILD_JOBS=1",
            "type=bind,source=/source,target=/project,readonly",
            IMAGE,
        ] {
            assert!(
                step.arguments.iter().any(|argument| argument == expected),
                "{expected}"
            );
        }
        assert!(step.budget.as_secs() <= 900);
        let file_limit = if matches!(phase, Phase::Build(_)) {
            "fsize=536870912:536870912"
        } else {
            "fsize=134217728:134217728"
        };
        assert!(step.arguments.iter().any(|argument| argument == file_limit));
        assert!(plan
            .cleanup(phase)
            .arguments
            .iter()
            .any(|argument| argument.starts_with("rusterisk-input-fuzz-fixture-")));
        assert_eq!(
            step.arguments
                .iter()
                .any(|argument| argument == "--network=bridge"),
            phase == Phase::Fetch
        );
    }
    let build = plan.step(Phase::Build(Target::Settings));
    for expected in [
        "fuzz",
        "build",
        "--build-std",
        "--sanitizer=address",
        "--dev",
        "--target=aarch64-unknown-linux-gnu",
    ] {
        assert!(
            build.arguments.iter().any(|argument| argument == expected),
            "{expected}"
        );
    }
    let run = plan.step(Phase::Run(Target::Settings));
    for expected in [
        "/state/target/aarch64-unknown-linux-gnu/debug/settings",
        "-seed=1",
        "-runs=100000",
        "-max_total_time=60",
        "-max_len=65536",
        "-timeout=2",
        "-rss_limit_mb=1024",
        "-print_final_stats=1",
        "--env=ASAN_OPTIONS=detect_leaks=1:halt_on_error=1:abort_on_error=1:detect_odr_violation=1",
    ] {
        assert!(
            run.arguments.iter().any(|argument| argument == expected),
            "{expected}"
        );
    }
    assert!(!run.arguments.iter().any(|argument| argument == "cargo"));
    assert_eq!(run.budget.as_secs(), 90);
    Ok(())
}

#[test]
fn fuzz_plans_reject_mutable_images_or_unsafe_mount_and_container_names() {
    for (identity, source, state, image) in [
        ("fixture", "/source", "/state", "latest"),
        ("-other", "/source", "/state", IMAGE),
        ("fixture", "/source,readonly=false", "/state", IMAGE),
        ("fixture", "/source", "", IMAGE),
    ] {
        assert!(FuzzPlan::new(identity, source, state, image, LinuxTarget::Amd64).is_err());
    }
}

#[test]
fn fuzz_manifest_binds_each_harness_to_the_actual_workspace_dependencies() -> Result<(), FuzzError>
{
    let source = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fuzz/Cargo.toml"),
    )?;
    let normalized = normalize_manifest(&source)?;
    let manifest: toml::Value = toml::from_str(&normalized).map_err(FuzzError::Toml)?;
    assert_eq!(
        manifest["dependencies"]["rusterisk-api"]["path"].as_str(),
        Some("/project/crates/api")
    );
    assert_eq!(
        manifest["dependencies"]["rusterisk-api"]
            .get("version")
            .and_then(toml::Value::as_str),
        Some(concat!("=", env!("CARGO_PKG_VERSION")))
    );
    let pin = format!("version = \"={}\"", env!("CARGO_PKG_VERSION"));
    for invalid in [
        String::new(),
        "[workspace]\n".to_owned(),
        source.replace("../../crates/api", "../../crates/fake"),
        source.replace("=0.4.13", "0.4"),
        source.replace(&format!("{pin}, "), ""),
        source.replace(&pin, "version = \"*\""),
        source.replace("fuzz_targets/settings.rs", "fuzz_targets/unused.rs"),
        source.replace("name = \"sip\"", "name = \"unknown\""),
    ] {
        assert!(normalize_manifest(&invalid).is_err());
    }
    Ok(())
}

#[test]
fn fuzz_vendor_patch_is_rebased_only_from_the_owned_project_relative_path() -> Result<(), FuzzError>
{
    let mut source: toml::Value =
        toml::from_str(include_str!("../fuzz/Cargo.toml")).map_err(FuzzError::Toml)?;
    source
        .as_table_mut()
        .ok_or(FuzzError::Manifest)?
        .remove("patch");
    let source = toml::to_string(&source).map_err(|_| FuzzError::Manifest)?;
    let patched = format!("{source}\n[patch.crates-io]\nstr0m={{path='../../vendor/str0m'}}\n");
    let normalized: toml::Value =
        toml::from_str(&normalize_manifest(&patched)?).map_err(FuzzError::Toml)?;
    assert_eq!(
        normalized["patch"]["crates-io"]["str0m"]["path"].as_str(),
        Some("/project/vendor/str0m")
    );
    for patch in [
        "str0m={path='../vendor/str0m'}",
        "str0m={path='../../vendor/other'}",
        "str0m={path='/project/vendor/str0m'}",
        "str0m={path=42}",
        "str0m={git='https://example.invalid/str0m'}",
        "other={path='../../vendor/str0m'}",
    ] {
        assert!(normalize_manifest(&format!("{source}\n[patch.crates-io]\n{patch}\n")).is_err());
    }
    Ok(())
}

#[test]
fn native_fuzz_discovery_and_completion_refuse_empty_or_incomplete_runs() -> Result<(), FuzzError> {
    validate_inventory(b"media_controls\nmedia_packets\nsettings\nsip\n")?;
    for invalid in [
        b"".as_slice(),
        b"settings\n",
        b"media_controls\nmedia_packets\nsettings\nsip\nsip\n",
        b"media_controls\nmedia_packets\nsettings\nsip\nunknown\n",
    ] {
        assert!(validate_inventory(invalid).is_err());
    }
    let complete = "#100 DONE cov: 45 ft: 70 corp: 10/1Kb\nDone 100 runs in 1 second(s)\nstat::number_of_executed_units: 100\nstat::peak_rss_mb: 40\n";
    assert_eq!(validate_run(complete.as_bytes(), 7)?, 100);
    let native = complete.replace("#100 DONE ", "#100\tDONE   ");
    assert_eq!(validate_run(native.as_bytes(), 7)?, 100);
    for invalid in [
        String::new(),
        complete.replace("100", "0"),
        complete.replace(
            "stat::number_of_executed_units: 100",
            "stat::number_of_executed_units: 99",
        ),
        complete.replace(" cov: 45", " cov: 0"),
        complete.replace("Done 100 runs in 1 second(s)\n", ""),
        format!("{complete}==1==ERROR: AddressSanitizer: heap-buffer-overflow\n"),
        format!("{complete}libFuzzer: timeout after 2 seconds\n"),
        format!("{complete}stat::number_of_executed_units: 100\n"),
    ] {
        assert!(validate_run(invalid.as_bytes(), 7).is_err(), "{invalid}");
    }
    Ok(())
}

#[test]
fn fuzz_locks_preserve_production_dependency_identities() -> Result<(), FuzzError> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    xtask::input_fuzz::validate_locks(
        &std::fs::read_to_string(root.join("../Cargo.lock"))?,
        &std::fs::read_to_string(root.join("fuzz/Cargo.lock"))?,
    )?;
    let production = "[[package]]\nname='serde'\nversion='1.0.229'\nsource='registry+https://github.com/rust-lang/crates.io-index'\nchecksum='same'\n";
    for invalid in [
        String::new(),
        production.replace("1.0.229", "1.0.228"),
        production.replace("same", "changed"),
        production.replace("serde", "unknown"),
    ] {
        assert!(xtask::input_fuzz::validate_locks(production, &invalid).is_err());
    }
    Ok(())
}
