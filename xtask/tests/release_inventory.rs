use xtask::{
    release_check::{LinuxOciTarget, RequiredGate},
    server_distribution::ServerTarget,
};

#[test]
fn candidate_verification_requires_the_complete_approved_inventory() {
    let required = RequiredGate::ALL;
    assert_eq!(required.len(), 17);
    for (index, gate) in required.iter().enumerate() {
        assert!(!required[..index].contains(gate));
    }
    for gate in [
        RequiredGate::Fast,
        RequiredGate::Security,
        RequiredGate::Reference,
        RequiredGate::Browser,
        RequiredGate::CoreProofs,
        RequiredGate::CoreMiri,
        RequiredGate::CoreMutation,
        RequiredGate::InputFuzz,
        RequiredGate::NativeSanitizers,
        RequiredGate::Sdk,
    ] {
        assert!(required.contains(&gate));
    }
    for target in ServerTarget::ALL {
        assert!(required.contains(&RequiredGate::Server(target)));
    }
    for (platform, target) in [
        (LinuxOciTarget::X86_64, ServerTarget::LinuxX86_64),
        (LinuxOciTarget::Arm64, ServerTarget::LinuxArm64),
    ] {
        assert!(required.contains(&RequiredGate::Oci(platform)));
        assert_eq!(platform.server_target(), target);
    }
}
