use super::bind_capture;
use crate::{
    core_proofs::{artifact_fixture::using_fixture, CapturedCoreProofs, ProofError},
    release_check::{CapturedReleaseSource, SourceInputs},
};
use std::{fs, io, path::Path};

#[test]
fn proof_execution_capture_stays_bound_after_the_original_files_are_restored() -> io::Result<()> {
    using_fixture(|fixture| {
        let project = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .ok_or_else(|| io::Error::other("Proof project directory is missing"))?;
        let captured = CapturedReleaseSource::read(project).map_err(io::Error::other)?;
        let source = fixture.root.join("release-source");
        captured
            .write(&source, &fixture.root.join("captured-state"))
            .map_err(io::Error::other)?;
        let matching = CapturedCoreProofs::read(&source).map_err(io::Error::other)?;
        bind_capture(&captured, &matching).map_err(io::Error::other)?;
        let path = source.join("crates/core/src/lib.rs");
        let original = fs::read(&path)?;
        let mut changed = original.clone();
        changed.push(b'\n');
        fs::write(&path, changed)?;
        let different = CapturedCoreProofs::read(&source).map_err(io::Error::other)?;
        fs::write(&path, original)?;
        captured.check(&source).map_err(io::Error::other)?;
        assert_ne!(
            SourceInputs::new(matching.inputs())
                .map_err(io::Error::other)?
                .key(),
            SourceInputs::new(different.inputs())
                .map_err(io::Error::other)?
                .key()
        );
        assert!(matches!(
            bind_capture(&captured, &different),
            Err(ProofError::SourceChanged)
        ));
        let restored = CapturedCoreProofs::read(&source).map_err(io::Error::other)?;
        bind_capture(&captured, &restored).map_err(io::Error::other)
    })
}
