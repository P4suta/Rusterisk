use crate::{
    input_fuzz::LinuxTarget,
    reference::ReferenceImage,
    release_check::{CandidateImages, CandidateProfile},
};
use std::io;

#[derive(Clone, Copy, Debug)]
enum ImageRole {
    Browser,
    CoreProofs,
    CoreMiri,
    CoreMutation,
    InputFuzz,
    NativeSanitizers,
}

impl ImageRole {
    const ALL: [Self; 6] = [
        Self::Browser,
        Self::CoreProofs,
        Self::CoreMiri,
        Self::CoreMutation,
        Self::InputFuzz,
        Self::NativeSanitizers,
    ];

    fn images<'a>(
        self,
        reference: &'a ReferenceImage,
        selected: &'a str,
        baseline: &'a str,
    ) -> CandidateImages<'a> {
        let mut images = CandidateImages {
            reference,
            browser: baseline,
            core_proofs: baseline,
            core_miri: baseline,
            core_mutation: baseline,
            input_fuzz: baseline,
            native_sanitizers: baseline,
        };
        match self {
            Self::Browser => images.browser = selected,
            Self::CoreProofs => images.core_proofs = selected,
            Self::CoreMiri => images.core_miri = selected,
            Self::CoreMutation => images.core_mutation = selected,
            Self::InputFuzz => images.input_fuzz = selected,
            Self::NativeSanitizers => images.native_sanitizers = selected,
        }
        images
    }
}

fn checked_reference() -> io::Result<ReferenceImage> {
    ReferenceImage::parse_inspection(&format!("sha256:{}\n", "a".repeat(64)))
        .map_err(io::Error::other)
}

#[test]
fn each_string_image_role_rejects_mutable_empty_and_malformed_selections() -> io::Result<()> {
    let reference = checked_reference()?;
    let baseline = format!("sha256:{}", "a".repeat(64));
    let invalid = [
        String::new(),
        "ghcr.io/p4suta/rusterisk:latest".into(),
        "ghcr.io/p4suta/rusterisk".into(),
        "sha256:".into(),
        "sha256:short".into(),
        format!("sha256:{}", "a".repeat(63)),
        format!("sha256:{}", "a".repeat(65)),
        format!("sha256:{}", "g".repeat(64)),
        format!("sha512:{}", "a".repeat(128)),
        format!("ghcr.io/p4suta/rusterisk@sha256:{}", "g".repeat(64)),
        format!("sha256:{}\n", "a".repeat(64)),
    ];
    for role in ImageRole::ALL {
        for value in &invalid {
            let result = CandidateProfile::new(
                role.images(&reference, value, &baseline),
                LinuxTarget::Arm64,
            );
            assert!(
                result.is_err(),
                "{role:?} accepted mutable or malformed image {value:?}"
            );
        }
    }
    Ok(())
}

#[test]
fn each_string_image_role_accepts_canonical_immutable_ids_and_references() -> io::Result<()> {
    let reference = checked_reference()?;
    let baseline = format!("sha256:{}", "a".repeat(64));
    let valid = [
        format!("sha256:{}", "b".repeat(64)),
        format!("ghcr.io/p4suta/rusterisk@sha256:{}", "c".repeat(64)),
        format!("ghcr.io/p4suta/rusterisk:0.1.0@sha256:{}", "d".repeat(64)),
    ];
    for role in ImageRole::ALL {
        for value in &valid {
            let result = CandidateProfile::new(
                role.images(&reference, value, &baseline),
                LinuxTarget::Amd64,
            );
            assert!(
                result.is_ok(),
                "{role:?} rejected canonical immutable image {value:?}"
            );
        }
    }
    Ok(())
}

fn owned_selection() -> io::Result<CandidateProfile> {
    let reference = checked_reference()?;
    let browser = format!("sha256:{}", "1".repeat(64));
    let proofs = format!("sha256:{}", "2".repeat(64));
    let miri = format!("sha256:{}", "3".repeat(64));
    let mutation = format!("sha256:{}", "4".repeat(64));
    let fuzz = format!("sha256:{}", "5".repeat(64));
    let native = format!("sha256:{}", "6".repeat(64));
    CandidateProfile::new(
        CandidateImages {
            reference: &reference,
            browser: &browser,
            core_proofs: &proofs,
            core_miri: &miri,
            core_mutation: &mutation,
            input_fuzz: &fuzz,
            native_sanitizers: &native,
        },
        LinuxTarget::Arm64,
    )
    .map_err(io::Error::other)
}

#[test]
fn a_profile_outlives_every_borrowed_image_input() -> io::Result<()> {
    let profile = owned_selection()?;
    drop(profile);
    Ok(())
}
