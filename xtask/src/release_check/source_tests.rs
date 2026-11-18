use super::{add, ReleaseSourceError, Remaining};
use crate::distribution::SourceSnapshot;
use std::{
    fs, io,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

const SHARED: &str = "tests/interop/node-ari-client/project-service-only/index.ts";

fn using(test: impl FnOnce(&SourceSnapshot, &Path) -> io::Result<()>) -> io::Result<()> {
    let project = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or_else(|| io::Error::other("Workspace is missing"))?;
    let snapshot = SourceSnapshot::read(project).map_err(io::Error::other)?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    let root = fs::canonicalize(std::env::temp_dir())?.join(format!(
        "rusterisk-shared-release-inputs-{}-{stamp}",
        std::process::id()
    ));
    fs::create_dir(&root)?;
    let source = root.join("source");
    let operation = snapshot
        .write(&source, &root.join("state"))
        .map_err(io::Error::other)
        .and_then(|()| test(&snapshot, &source));
    match (operation, fs::remove_dir_all(root)) {
        (operation, Ok(())) => operation,
        (Ok(()), Err(error)) => Err(error),
        (Err(primary), Err(cleanup)) => Err(io::Error::other(format!(
            "{primary}; shared source fixture cleanup failed: {cleanup}"
        ))),
    }
}

#[test]
fn shared_release_inputs_use_one_inventory_slot_and_no_additional_byte_budget() -> io::Result<()> {
    using(|snapshot, source| {
        let mut remaining = Remaining {
            files: 0,
            bytes: 0,
            distribution: snapshot,
        };
        let mut extra = Vec::new();
        add(source, Path::new(SHARED), &mut extra, &mut remaining).map_err(io::Error::other)?;
        assert!(extra.is_empty());
        assert_eq!((remaining.files, remaining.bytes), (0, 0));
        let additional = Path::new("additional.rs");
        fs::write(
            source.join(additional),
            b"pub const ADDITIONAL: bool = true;\n",
        )?;
        assert!(matches!(
            add(source, additional, &mut extra, &mut remaining),
            Err(ReleaseSourceError::Inputs(
                crate::release_check::InputError::Limit
            ))
        ));
        assert!(extra.is_empty());
        Ok(())
    })
}

#[test]
fn changed_shared_release_input_bytes_fail_without_consuming_the_extra_budget() -> io::Result<()> {
    using(|snapshot, source| {
        let mut bytes = fs::read(source.join(SHARED))?;
        bytes.push(b'\n');
        fs::write(source.join(SHARED), bytes)?;
        let mut remaining = Remaining {
            files: 1,
            bytes: 32,
            distribution: snapshot,
        };
        let mut extra = Vec::new();
        assert!(matches!(
            add(source, Path::new(SHARED), &mut extra, &mut remaining),
            Err(ReleaseSourceError::Changed)
        ));
        assert!(extra.is_empty());
        assert_eq!((remaining.files, remaining.bytes), (1, 32));
        Ok(())
    })
}

#[cfg(unix)]
#[test]
fn a_shared_input_link_cannot_reuse_an_earlier_regular_file_capture() -> io::Result<()> {
    using(|snapshot, source| {
        let original = source.join(SHARED);
        let foreign = source.join("foreign.ts");
        fs::rename(&original, &foreign)?;
        std::os::unix::fs::symlink(&foreign, &original)?;
        let mut remaining = Remaining {
            files: 1,
            bytes: 32,
            distribution: snapshot,
        };
        let mut extra = Vec::new();
        assert!(matches!(
            add(source, Path::new(SHARED), &mut extra, &mut remaining),
            Err(ReleaseSourceError::Invalid)
        ));
        assert!(fs::symlink_metadata(original)?.file_type().is_symlink());
        assert!(extra.is_empty());
        Ok(())
    })
}
