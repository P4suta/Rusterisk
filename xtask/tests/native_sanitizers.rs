use xtask::native_sanitizers::{NativeSanitizerPlan, Phase, Scope};

const IMAGE: &str = "sha256:8e26e6eeb29b66c3849295c5606adaed78a2c6bd9ef17644370256c7fa977d20";

#[test]
fn native_sanitizers_rebuild_real_dependencies_in_bounded_isolated_containers(
) -> Result<(), std::io::Error> {
    let plan = NativeSanitizerPlan::new("fixture", "/source", "/state", IMAGE)?;
    for phase in [
        Phase::Fetch,
        Phase::Build(Scope::Media),
        Phase::Build(Scope::Storage),
        Phase::Build(Scope::Tls),
        Phase::Run(Scope::Media),
        Phase::Run(Scope::Storage),
        Phase::Run(Scope::Tls),
    ] {
        let step = plan.step(phase);
        let args = &step.arguments;
        for required in [
            "--rm",
            "--pull=never",
            "--cpus=1",
            "--memory=2g",
            "--pids-limit=256",
            "--read-only",
            "--cap-drop=ALL",
            "--security-opt=no-new-privileges",
            "--entrypoint=/usr/bin/timeout",
            IMAGE,
        ] {
            assert!(
                args.iter().any(|argument| argument == required),
                "{required}: {args:?}"
            );
        }
        assert!(args
            .iter()
            .any(|argument| argument == "type=bind,source=/source,target=/project,readonly"));
        assert!(args.iter().any(|argument| argument == "--env=CFLAGS=-fsanitize=address,undefined -fno-sanitize-recover=all -fno-omit-frame-pointer -g"));
        assert!(args.iter().any(|argument| argument
            == "--env=RUSTFLAGS=-C linker=clang -C link-arg=-fsanitize=address,undefined"));
        assert!(args
            .iter()
            .any(|argument| argument == "--env=CARGO_BUILD_JOBS=1"));
        let network = if phase == Phase::Fetch {
            "--network=bridge"
        } else {
            "--network=none"
        };
        assert!(args.iter().any(|argument| argument == network));
        assert!(step.budget.as_secs() <= 900);
        assert_eq!(plan.cleanup(phase).arguments[0], "rm");
    }
    Ok(())
}

#[test]
fn native_sanitizer_plans_reject_mutable_images_and_ambiguous_mounts() {
    for image in ["latest", "rust:1.99.0", "sha256:short"] {
        assert!(NativeSanitizerPlan::new("fixture", "/source", "/state", image).is_err());
    }
    for (source, state) in [
        ("source", "/state"),
        ("/source", "/source"),
        ("/source", "/source/state"),
        ("/source,bad", "/state"),
    ] {
        assert!(NativeSanitizerPlan::new("fixture", source, state, IMAGE).is_err());
    }
}

#[test]
fn native_sanitizers_probe_the_runtime_and_discover_tests_before_execution(
) -> Result<(), std::io::Error> {
    use xtask::native_sanitizers::Defect;
    let plan = NativeSanitizerPlan::new("fixture", "/source", "/state", IMAGE)?;
    assert!(plan
        .step(Phase::ClangVersion)
        .arguments
        .iter()
        .any(|argument| argument == "clang"));
    assert!(plan
        .step(Phase::RustVersion)
        .arguments
        .iter()
        .any(|argument| argument == "rustc"));
    for defect in [Defect::Address, Defect::Undefined] {
        let compile = plan.step(Phase::CompileProbe(defect));
        assert!(compile.arguments.iter().any(|argument| argument == "clang"));
        assert!(compile
            .arguments
            .iter()
            .any(|argument| argument == "-fsanitize=address,undefined"));
        let rejection = plan.step(Phase::RejectProbe(defect));
        assert!(rejection
            .arguments
            .iter()
            .any(|argument| argument == "--network=none"));
        assert!(rejection
            .arguments
            .iter()
            .any(|argument| argument.starts_with("/state/probe-")));
    }
    for scope in Scope::ALL {
        let discovery = plan.step(Phase::Inventory(scope));
        assert!(discovery
            .arguments
            .iter()
            .any(|argument| argument == "--list"));
        assert!(!discovery
            .arguments
            .iter()
            .any(|argument| argument == "--no-run"));
    }
    Ok(())
}
