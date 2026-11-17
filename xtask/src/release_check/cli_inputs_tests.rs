use super::cli_inputs::{ReleaseInputError, ReleaseInputs, ReleaseRequest};
use crate::{
    input_fuzz::LinuxTarget,
    reference::ReferenceImage,
    release_check::{CandidateProfile, LinuxOciTarget, RequiredGate},
    server_distribution::{ServerExecutionMode, ServerTarget},
};
use std::{io, path::PathBuf};

const IMAGES: [(RequiredGate, char); 7] = [
    (RequiredGate::Reference, 'a'),
    (RequiredGate::Browser, 'b'),
    (RequiredGate::CoreProofs, 'c'),
    (RequiredGate::CoreMiri, 'd'),
    (RequiredGate::CoreMutation, 'e'),
    (RequiredGate::InputFuzz, 'f'),
    (RequiredGate::NativeSanitizers, '0'),
];
const CHECKS: [RequiredGate; 7] = [
    RequiredGate::Reference,
    RequiredGate::Browser,
    RequiredGate::CoreProofs,
    RequiredGate::CoreMiri,
    RequiredGate::CoreMutation,
    RequiredGate::InputFuzz,
    RequiredGate::NativeSanitizers,
];

fn input_file() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("release-input-fixture")
        .join("inputs.toml")
}

fn image(digit: char) -> String {
    format!("sha256:{}", digit.to_string().repeat(64))
}

fn server(target: ServerTarget, execution: &str) -> String {
    let target = target.triple();
    format!(
        "[servers.{target}]\ncandidate = \"candidates/server-{target}\"\nartifact = \"artifacts/server-{target}\"\nexecution = \"{execution}\"\n"
    )
}

fn oci(target: LinuxOciTarget) -> String {
    let target = target.server_target().triple();
    format!(
        "[oci.{target}]\narchive = \"candidates/oci-{target}.tar\"\nartifact = \"artifacts/oci-{target}\"\n"
    )
}

fn document() -> String {
    let mut document = String::from(
        "format = 1\nsource = \"captured-source\"\nfuzz_architecture = \"aarch64-unknown-linux-gnu\"\n[images]\n",
    );
    for (gate, digit) in IMAGES {
        document.push_str(&format!("{gate} = \"{}\"\n", image(digit)));
    }
    document.push_str(
        "[sdk]\ncandidate = \"candidates/sdk\"\nartifact = \"artifacts/sdk\"\n[checks]\n",
    );
    for gate in CHECKS {
        document.push_str(&format!("{gate} = \"artifacts/{gate}\"\n"));
    }
    for target in ServerTarget::ALL {
        document.push_str(&server(target, "native"));
    }
    for target in [LinuxOciTarget::X86_64, LinuxOciTarget::Arm64] {
        document.push_str(&oci(target));
    }
    document
}

fn parse(bytes: &[u8]) -> Result<ReleaseInputs, ReleaseInputError> {
    ReleaseInputs::parse(bytes, &input_file())
}

fn checked<T>(result: Result<T, impl std::fmt::Display>) -> io::Result<T> {
    result.map_err(|error| io::Error::other(error.to_string()))
}

#[test]
fn complete_inputs_bind_the_fixed_inventory_profile_and_candidate_paths() -> io::Result<()> {
    let selected = checked(parse(document().as_bytes()))?;
    let file = input_file();
    let base = file
        .parent()
        .ok_or_else(|| io::Error::other("Input fixture has no parent"))?;
    assert_eq!(selected.source(), base.join("captured-source"));
    assert_eq!(selected.sdk_candidate(), base.join("candidates/sdk"));
    let profile: &CandidateProfile = selected.profile();
    let _: &ReferenceImage = selected.reference_image();
    assert_eq!(selected.reference_image().as_str(), image('a'));
    assert_eq!(profile.fuzz_architecture(), LinuxTarget::Arm64);
    for (gate, digit) in IMAGES {
        assert_eq!(profile.image(gate), Some(image(digit).as_str()));
    }
    for gate in RequiredGate::ALL {
        let expected = match gate {
            RequiredGate::Fast | RequiredGate::Security => None,
            RequiredGate::Reference
            | RequiredGate::Browser
            | RequiredGate::CoreProofs
            | RequiredGate::CoreMiri
            | RequiredGate::CoreMutation
            | RequiredGate::InputFuzz
            | RequiredGate::NativeSanitizers
            | RequiredGate::Sdk
            | RequiredGate::Server(_)
            | RequiredGate::Oci(_) => {
                Some(base.join(format!("artifacts/{}", gate.to_string().replace(':', "-"))))
            }
        };
        assert_eq!(selected.artifact(gate), expected.as_deref());
    }
    for target in ServerTarget::ALL {
        assert_eq!(
            selected.server_candidate(target),
            base.join(format!("candidates/server-{}", target.triple()))
        );
        assert_eq!(
            selected.server_execution(target),
            ServerExecutionMode::Native
        );
    }
    for target in [LinuxOciTarget::X86_64, LinuxOciTarget::Arm64] {
        assert_eq!(
            selected.oci_archive(target),
            base.join(format!(
                "candidates/oci-{}.tar",
                target.server_target().triple()
            ))
        );
    }
    Ok(())
}

#[test]
fn inputs_reject_unknown_formats_fields_duplicates_and_missing_inventory() {
    let valid = document();
    for invalid in [
        valid.replacen("format = 1\n", "", 1),
        valid.replacen("format = 1", "format = 0", 1),
        valid.replacen("format = 1", "format = 2", 1),
        format!("format = 1\n{valid}"),
        format!("unexpected = true\n{valid}"),
        valid.replace("[images]\n", "[images]\nunexpected = \"image\"\n"),
        valid.replace("[checks]\n", "[checks]\nunexpected = \"artifacts/other\"\n"),
        valid.replace("[checks]\n", "[checks]\nfast = \"artifacts/fast\"\n"),
        valid.replace("[checks]\n", "[checks]\nsecurity = \"artifacts/security\"\n"),
        valid.replace(
            "[sdk]\ncandidate = \"candidates/sdk\"\nartifact = \"artifacts/sdk\"\n",
            "",
        ),
        format!("{valid}[servers.riscv64-unknown-linux-gnu]\ncandidate = \"other\"\nartifact = \"other\"\nexecution = \"native\"\n"),
        format!("{valid}[oci.x86_64-pc-windows-msvc]\narchive = \"other.tar\"\nartifact = \"other\"\n"),
        valid.replace(
            "[oci.x86_64-unknown-linux-gnu]\n",
            "[oci.x86_64-unknown-linux-gnu]\ncandidate = \"another-server\"\n",
        ),
    ] {
        assert!(parse(invalid.as_bytes()).is_err());
    }
    for (gate, digit) in IMAGES {
        let field = format!("{gate} = \"{}\"\n", image(digit));
        assert!(
            parse(valid.replace(&field, "").as_bytes()).is_err(),
            "{gate}"
        );
        assert!(
            parse(valid.replace(&field, &format!("{field}{field}")).as_bytes()).is_err(),
            "{gate}"
        );
    }
    for gate in CHECKS {
        let field = format!("{gate} = \"artifacts/{gate}\"\n");
        assert!(
            parse(valid.replace(&field, "").as_bytes()).is_err(),
            "{gate}"
        );
    }
    for target in ServerTarget::ALL {
        assert!(
            parse(valid.replace(&server(target, "native"), "").as_bytes()).is_err(),
            "{}",
            target.triple()
        );
    }
    for target in [LinuxOciTarget::X86_64, LinuxOciTarget::Arm64] {
        assert!(parse(valid.replace(&oci(target), "").as_bytes()).is_err());
    }
}

#[test]
fn input_bytes_and_paths_obey_their_bounds_before_execution() -> io::Result<()> {
    let valid = document();
    assert!(parse(&[0xff]).is_err());
    assert!(parse(&[]).is_err());
    let mut limit = valid.clone();
    limit.push('#');
    limit.push_str(&"x".repeat(64 * 1024 - limit.len()));
    assert_eq!(limit.len(), 64 * 1024);
    checked(parse(limit.as_bytes()))?;
    limit.push('x');
    assert!(parse(limit.as_bytes()).is_err());
    for path in [
        String::new(),
        String::from("\\u0000"),
        String::from("\\n"),
        "x".repeat(4097),
    ] {
        let invalid = valid.replacen(
            "source = \"captured-source\"",
            &format!("source = \"{path}\""),
            1,
        );
        assert!(parse(invalid.as_bytes()).is_err());
    }
    let maximum_path = valid.replacen(
        "source = \"captured-source\"",
        &format!("source = \"{}\"", "x".repeat(4096)),
        1,
    );
    checked(parse(maximum_path.as_bytes()))?;
    Ok(())
}

#[test]
fn release_requests_select_one_gate_or_complete_retained_validation() -> io::Result<()> {
    assert!(matches!(
        checked(ReleaseRequest::parse(&["--dry-run"]))?,
        ReleaseRequest::Inventory
    ));
    for gate in RequiredGate::ALL {
        let name = gate.to_string();
        let request = checked(ReleaseRequest::parse(&[
            "run",
            &name,
            "--inputs",
            "input path.toml",
        ]))?;
        assert!(
            matches!(request, ReleaseRequest::Run { gate: selected, inputs }
            if selected == gate && inputs == PathBuf::from("input path.toml"))
        );
    }
    let request = checked(ReleaseRequest::parse(&[
        "verify",
        "--inputs",
        "inputs.toml",
    ]))?;
    assert!(matches!(request, ReleaseRequest::Verify { inputs }
        if inputs == PathBuf::from("inputs.toml")));
    for invalid in [
        &[][..],
        &["--run"][..],
        &["run", "fast"][..],
        &["run", "unknown", "--inputs", "inputs.toml"][..],
        &[
            "run",
            "server:x86_64-unknown-freebsd",
            "--inputs",
            "inputs.toml",
        ][..],
        &[
            "run",
            "oci:x86_64-pc-windows-msvc",
            "--inputs",
            "inputs.toml",
        ][..],
        &["run", "fast", "--inputs", ""][..],
        &["verify", "--inputs"][..],
        &["verify", "--inputs", "inputs.toml", "--run"][..],
        &["--dry-run", "--inputs", "inputs.toml"][..],
        &[
            "run",
            "fast",
            "--inputs",
            "first.toml",
            "--inputs",
            "second.toml",
        ][..],
    ] {
        assert!(ReleaseRequest::parse(invalid).is_err(), "{invalid:?}");
    }
    let extra = [
        "run",
        "fast",
        "--inputs",
        "inputs.toml",
        "trailing",
        "trailing",
        "trailing",
        "trailing",
        "trailing",
        "trailing",
        "trailing",
        "trailing",
        "trailing",
    ];
    assert_eq!(extra.len(), 13);
    assert!(ReleaseRequest::parse(&extra).is_err());
    Ok(())
}

#[test]
fn profiles_require_immutable_images_supported_architecture_and_explicit_rosetta() -> io::Result<()>
{
    let valid = document();
    for (gate, digit) in IMAGES {
        let invalid = valid.replace(
            &format!("{gate} = \"{}\"", image(digit)),
            &format!("{gate} = \"mutable:tag\""),
        );
        assert!(parse(invalid.as_bytes()).is_err(), "{gate}");
    }
    let amd64 = valid.replace(
        "fuzz_architecture = \"aarch64-unknown-linux-gnu\"",
        "fuzz_architecture = \"x86_64-unknown-linux-gnu\"",
    );
    assert_eq!(
        checked(parse(amd64.as_bytes()))?
            .profile()
            .fuzz_architecture(),
        LinuxTarget::Amd64
    );
    let unsupported = valid.replace(
        "fuzz_architecture = \"aarch64-unknown-linux-gnu\"",
        "fuzz_architecture = \"x86_64-pc-windows-msvc\"",
    );
    assert!(parse(unsupported.as_bytes()).is_err());
    for target in ServerTarget::ALL {
        let selected = valid.replace(&server(target, "native"), &server(target, "rosetta"));
        if target == ServerTarget::MacOsX86_64 {
            assert_eq!(
                checked(parse(selected.as_bytes()))?.server_execution(target),
                ServerExecutionMode::Rosetta
            );
        } else {
            assert!(parse(selected.as_bytes()).is_err(), "{}", target.triple());
        }
        let unsupported = valid.replace(&server(target, "native"), &server(target, "emulated"));
        assert!(
            parse(unsupported.as_bytes()).is_err(),
            "{}",
            target.triple()
        );
        let missing = valid.replace(
            &server(target, "native"),
            &server(target, "native").replace("execution = \"native\"\n", ""),
        );
        assert!(parse(missing.as_bytes()).is_err(), "{}", target.triple());
    }
    Ok(())
}
