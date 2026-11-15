use xtask::{
    core_assurance::{CoreAssurancePlan, Phase},
    core_proofs::CoreProofPlan,
    input_fuzz::{FuzzPlan, LinuxTarget},
};

#[test]
fn campaigns_accept_local_content_ids_and_repository_manifest_digests() -> Result<(), String> {
    for image in [
        "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        "registry.example/tools@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
    ] {
        let proofs = CoreProofPlan::new("fixture", "/source", "/state", image).map_err(|error| error.to_string())?;
        assert!(proofs.run().arguments.iter().any(|value| value == image));
        let assurance = CoreAssurancePlan::new("fixture", "/source", "/state", image).map_err(|error| error.to_string())?;
        assert!(assurance.step(Phase::MiriTests).arguments.iter().any(|value| value == image));
        let fuzz = FuzzPlan::new("fixture", "/source", "/state", image, LinuxTarget::Arm64).map_err(|error| error.to_string())?;
        assert!(fuzz.step(xtask::input_fuzz::Phase::Inventory).arguments.iter().any(|value| value == image));
    }
    Ok(())
}

#[test]
fn campaigns_refuse_mutable_names_incomplete_digests_and_option_like_images() {
    for image in [
        "latest",
        "registry.example/tools:latest",
        "sha256:0123",
        "sha256:zzzz",
        "@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        "--privileged@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
    ] {
        assert!(CoreProofPlan::new("fixture", "/source", "/state", image).is_err());
        assert!(CoreAssurancePlan::new("fixture", "/source", "/state", image).is_err());
        assert!(FuzzPlan::new("fixture", "/source", "/state", image, LinuxTarget::Arm64).is_err());
    }
}
