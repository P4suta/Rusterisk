use super::{outputs, require, using, NativeArtifactPublication, PublishedNativeArtifact, OUTPUTS};
use std::{fs, io, os::unix::fs::symlink};

#[test]
fn publication_preserves_an_existing_destination() -> io::Result<()> {
    using(|fixture| {
        fs::create_dir(&fixture.published)?;
        fs::write(fixture.published.join("foreign"), b"preserved\n")?;
        require(
            NativeArtifactPublication::create(&fixture.staging, &fixture.published).is_err(),
            "Native publication accepted an existing destination",
        )?;
        require(
            !fixture.staging.exists()
                && fs::read(fixture.published.join("foreign"))? == b"preserved\n",
            "Rejected native publication changed another artifact",
        )
    })
}

#[test]
fn publication_rechecks_its_destination_after_cleanup() -> io::Result<()> {
    using(|fixture| {
        let publication = NativeArtifactPublication::create(&fixture.staging, &fixture.published)?;
        outputs(publication.staging())?;
        let completed = (|| {
            fs::create_dir(&fixture.published)?;
            fs::write(fixture.published.join("foreign"), b"preserved\n")
        })();
        let result = publication.publish_completed(completed);
        require(
            result.is_err()
                && fs::read(fixture.published.join("foreign"))? == b"preserved\n"
                && fixture.staging.exists(),
            "Native publication replaced a destination created during cleanup",
        )
    })
}

#[test]
fn reader_rejects_duplicate_and_escaping_inventory_paths() -> io::Result<()> {
    using(|fixture| {
        fs::create_dir(&fixture.published)?;
        outputs(&fixture.published)?;
        PublishedNativeArtifact::read(&fixture.published, &OUTPUTS)?;
        for expected in [
            &[][..],
            &[OUTPUTS[0], OUTPUTS[0]][..],
            &[OUTPUTS[0], "server/VERSION.stdout.txt"][..],
            &["/server/version.stdout.txt"][..],
            &["../version.stdout.txt"][..],
            &["server/../version.stdout.txt"][..],
            &["server/./version.stdout.txt"][..],
            &["server//version.stdout.txt"][..],
            &["server\\version.stdout.txt"][..],
            &["C:/version.stdout.txt"][..],
            &["server/version\n.stdout.txt"][..],
        ] {
            require(
                PublishedNativeArtifact::read(&fixture.published, expected).is_err(),
                "Native reader accepted an invalid expected inventory",
            )?;
        }
        Ok(())
    })
}

#[test]
fn reader_rejects_unexpected_regular_files() -> io::Result<()> {
    using(|fixture| {
        fs::create_dir(&fixture.published)?;
        outputs(&fixture.published)?;
        fs::write(fixture.published.join("unexpected"), b"extra output\n")?;
        require(
            PublishedNativeArtifact::read(&fixture.published, &OUTPUTS).is_err(),
            "Native reader accepted an undeclared output",
        )
    })
}

#[test]
fn reader_rejects_file_and_directory_symlinks() -> io::Result<()> {
    using(|fixture| {
        fs::create_dir(&fixture.published)?;
        outputs(&fixture.published)?;
        let outside = fixture.root.join("outside");
        fs::create_dir(&outside)?;
        fs::write(outside.join("version.stdout.txt"), b"outside output\n")?;
        fs::remove_file(fixture.published.join(OUTPUTS[0]))?;
        symlink(
            outside.join("version.stdout.txt"),
            fixture.published.join(OUTPUTS[0]),
        )?;
        require(
            PublishedNativeArtifact::read(&fixture.published, &OUTPUTS).is_err(),
            "Native reader followed an output file symlink",
        )?;
        fs::remove_file(fixture.published.join(OUTPUTS[0]))?;
        fs::remove_file(fixture.published.join(OUTPUTS[1]))?;
        fs::remove_dir(fixture.published.join("server"))?;
        symlink(&outside, fixture.published.join("server"))?;
        require(
            PublishedNativeArtifact::read(&fixture.published, &OUTPUTS).is_err(),
            "Native reader followed an inventory directory symlink",
        )
    })
}

#[test]
fn publication_rejects_nonregular_staged_outputs() -> io::Result<()> {
    using(|fixture| {
        let publication = NativeArtifactPublication::create(&fixture.staging, &fixture.published)?;
        outputs(publication.staging())?;
        let outside = fixture.root.join("outside");
        fs::write(&outside, b"outside output\n")?;
        fs::remove_file(fixture.staging.join(OUTPUTS[0]))?;
        symlink(&outside, fixture.staging.join(OUTPUTS[0]))?;
        require(
            publication.publish_completed(fs::read(&outside)).is_err()
                && !fixture.published.exists()
                && fs::read(&outside)? == b"outside output\n",
            "Native publication admitted a nonregular output or changed its target",
        )
    })
}

#[test]
fn reader_rejects_an_oversized_regular_output() -> io::Result<()> {
    using(|fixture| {
        fs::create_dir(&fixture.published)?;
        outputs(&fixture.published)?;
        fs::OpenOptions::new()
            .write(true)
            .open(fixture.published.join(OUTPUTS[0]))?
            .set_len(272 * 1024 * 1024 + 1)?;
        require(
            PublishedNativeArtifact::read(&fixture.published, &OUTPUTS).is_err(),
            "Native reader admitted an output above its file byte limit",
        )
    })
}
