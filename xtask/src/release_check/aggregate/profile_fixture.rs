use crate::{
    input_fuzz::LinuxTarget,
    reference::ReferenceImage,
    release_check::{CandidateImages, CandidateProfile},
};
use std::io;

pub(in crate::release_check) fn profile(
    reference: &ReferenceImage,
) -> io::Result<CandidateProfile> {
    let image = format!("sha256:{}", "a".repeat(64));
    CandidateProfile::new(
        CandidateImages {
            reference,
            browser: &image,
            core_proofs: &image,
            core_miri: &image,
            core_mutation: &image,
            input_fuzz: &image,
            native_sanitizers: &image,
        },
        LinuxTarget::Arm64,
    )
    .map_err(io::Error::other)
}
