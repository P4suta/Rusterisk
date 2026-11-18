use super::{fixture::using_fixture, CommandMode};
use crate::{
    core_proofs::{validate_results, CoreProofOutcome, CoreProofPlan, ProofError},
    release_check::CapturedReleaseSource,
};
use std::{fs, io, path::Path, process::Command};

const IMAGE: &str = super::fixture::IMAGE;

fn require(valid: bool, reason: &'static str) -> io::Result<()> {
    if valid {
        Ok(())
    } else {
        Err(io::Error::other(reason))
    }
}

fn release_source() -> io::Result<CapturedReleaseSource> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or_else(|| io::Error::other("Workspace directory is missing"))?;
    CapturedReleaseSource::read(root).map_err(io::Error::other)
}

#[test]
fn invalid_images_cannot_start_retained_core_proofs_or_publish_artifacts() -> io::Result<()> {
    using_fixture(|fixture| {
        let captured = release_source()?;
        let published = fixture.root.join("published");
        let result: Result<CoreProofOutcome, ProofError> =
            captured.verify_core_proofs_artifact("not-a-digest", &published);
        require(
            result.is_err() && !published.exists() && fixture.runs == 0,
            "Invalid image selection started or published retained core proofs",
        )
    })
}

#[test]
fn retained_core_proofs_preserve_an_existing_publication_destination() -> io::Result<()> {
    using_fixture(|fixture| {
        let captured = release_source()?;
        let published = fixture.root.join("published");
        fs::create_dir(&published)?;
        let marker = published.join("foreign");
        fs::write(&marker, b"preserve existing publication")?;
        let result: Result<CoreProofOutcome, ProofError> =
            captured.verify_core_proofs_artifact(IMAGE, &published);
        require(
            result.is_err()
                && fs::read(marker)? == b"preserve existing publication"
                && fixture.runs == 0,
            "Retained core proofs changed a publication outside their custody",
        )
    })
}

#[test]
fn complete_native_json_without_successful_publication_remains_diagnostic() -> io::Result<()> {
    using_fixture(|fixture| {
        let captured = release_source()?;
        let missing = fixture.root.join("missing");
        let result: Result<CoreProofOutcome, ProofError> =
            captured.read_core_proofs_artifact(&missing, IMAGE);
        require(
            result.is_err() && !missing.exists(),
            "Core proof reader accepted or created missing artifacts",
        )?;
        let diagnostic = fixture.root.join("diagnostic");
        let results = diagnostic.join("results");
        fs::create_dir(&diagnostic)?;
        fs::create_dir(&results)?;
        let source = fixture.root.join("crates/core");
        let proof = CoreProofPlan::new(
            "diagnostic",
            source
                .to_str()
                .ok_or_else(|| io::Error::other("Proof source path is not UTF-8"))?,
            results
                .to_str()
                .ok_or_else(|| io::Error::other("Proof result path is not UTF-8"))?,
            IMAGE,
        )
        .map_err(io::Error::other)?;
        let step = proof.run();
        fixture
            .perform(
                Command::new("docker").args(&step.arguments),
                step.budget,
                CommandMode::Work,
            )
            .map_err(io::Error::other)?;
        let path = results.join("kani.json");
        let before = fs::read(&path)?;
        validate_results(&before).map_err(io::Error::other)?;
        let result: Result<CoreProofOutcome, ProofError> =
            captured.read_core_proofs_artifact(&diagnostic, IMAGE);
        require(
            result.is_err() && fs::read(path)? == before,
            "Core proof reader promoted or changed native diagnostic outputs",
        )
    })
}
