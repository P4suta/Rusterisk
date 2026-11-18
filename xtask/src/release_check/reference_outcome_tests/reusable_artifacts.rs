use super::{require, using_fixture};
use crate::release_check::{ReferenceOutcome, ReferenceRunError};
use std::{fs, io, path::PathBuf};

fn artifact(working: &std::path::Path) -> io::Result<PathBuf> {
    Ok(working
        .parent()
        .ok_or_else(|| io::Error::other("Reference fixture parent is missing"))?
        .canonicalize()?
        .join("published"))
}

#[test]
fn reference_artifact_production_preserves_an_existing_destination() -> io::Result<()> {
    using_fixture(|captured, image, fixture| {
        let published = artifact(&fixture.working)?;
        fs::create_dir(&published)?;
        let marker = published.join("foreign-marker");
        fs::write(&marker, b"preserve existing reference state")?;
        let result: Result<ReferenceOutcome<'_>, ReferenceRunError> =
            captured.verify_reference_artifact(image, &published);
        let error = result
            .err()
            .ok_or_else(|| io::Error::other("Reference production replaced existing state"))?;
        require(
            error.to_string().contains("destination")
                && fs::read(marker)? == b"preserve existing reference state",
            "Reference production lost its destination conflict or changed borrowed state",
        )
    })
}

#[test]
fn missing_and_partial_reference_artifacts_return_no_outcome() -> io::Result<()> {
    using_fixture(|captured, image, fixture| {
        let published = artifact(&fixture.working)?;
        let missing: Result<ReferenceOutcome<'_>, ReferenceRunError> =
            captured.read_reference_artifact(&published, image);
        require(
            missing.is_err() && !published.exists(),
            "Missing reference artifacts returned an outcome or created state",
        )?;
        fs::create_dir(&published)?;
        let inspection = published.join("reference-image.stdout.txt");
        let bytes = format!("{}\n", image.as_str());
        fs::write(&inspection, &bytes)?;
        let partial: Result<ReferenceOutcome<'_>, ReferenceRunError> =
            captured.read_reference_artifact(&published, image);
        require(
            partial.is_err() && fs::read(inspection)? == bytes.as_bytes(),
            "Partial reference diagnostics returned an outcome or changed native output",
        )
    })
}

#[test]
fn reference_artifact_production_rejects_a_relative_destination() -> io::Result<()> {
    using_fixture(|captured, image, fixture| {
        let name = fixture
            .working
            .parent()
            .and_then(std::path::Path::file_name)
            .ok_or_else(|| io::Error::other("Reference fixture identity is missing"))?;
        let published = PathBuf::from(name).join("published");
        require(
            !published.exists(),
            "Relative reference fixture destination already exists",
        )?;
        let result: Result<ReferenceOutcome<'_>, ReferenceRunError> =
            captured.verify_reference_artifact(image, &published);
        let error = result.err().ok_or_else(|| {
            io::Error::other("Relative destination published a reference outcome")
        })?;
        require(
            error.to_string().contains("absolute") && !published.exists(),
            "Reference production accepted or created a relative destination",
        )
    })
}

#[cfg(unix)]
#[test]
fn linked_reference_artifacts_preserve_the_foreign_directory() -> io::Result<()> {
    using_fixture(|captured, image, fixture| {
        let published = artifact(&fixture.working)?;
        let foreign = published.with_file_name("foreign");
        fs::create_dir(&foreign)?;
        let marker = foreign.join("foreign-marker");
        fs::write(&marker, b"preserve linked reference state")?;
        std::os::unix::fs::symlink(&foreign, &published)?;
        let result: Result<ReferenceOutcome<'_>, ReferenceRunError> =
            captured.read_reference_artifact(&published, image);
        require(
            result.is_err()
                && fs::symlink_metadata(published)?.file_type().is_symlink()
                && fs::read(marker)? == b"preserve linked reference state",
            "Reference artifact reading followed or changed a foreign directory link",
        )
    })
}
