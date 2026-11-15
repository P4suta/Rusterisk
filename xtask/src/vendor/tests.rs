use super::{digest, verify_tree, Record, ARCHIVE_SHA256};
use std::{
    collections::BTreeMap,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

fn fixture(
    operation: impl FnOnce(&Path) -> Result<(), super::VendorError>,
) -> Result<(), super::VendorError> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(std::io::Error::other)?
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "rusterisk-vendor-test-{}-{stamp}",
        std::process::id()
    ));
    std::fs::create_dir(&root)?;
    let outcome = operation(&root);
    let cleanup = std::fs::remove_dir_all(&root).map_err(super::VendorError::from);
    outcome.and(cleanup)
}

fn record() -> Record {
    Record {
        name: "str0m".into(),
        version: "0.25.0".into(),
        archive_sha256: ARCHIVE_SHA256.into(),
        files: BTreeMap::from([("sample.txt".into(), digest(b"upstream\n"))]),
    }
}

#[test]
fn reverse_patch_verifies_the_complete_upstream_tree_and_rejects_unrecorded_changes(
) -> Result<(), super::VendorError> {
    fixture(|root| {
        let source = root.join("source");
        std::fs::create_dir(&source)?;
        std::fs::write(source.join("sample.txt"), b"patched\n")?;
        let patch = root.join("fix.patch");
        std::fs::write(&patch, b"diff --git a/sample.txt b/sample.txt\n--- a/sample.txt\n+++ b/sample.txt\n@@ -1 +1 @@\n-upstream\n+patched\n")?;
        verify_tree(&source, &record(), &patch)?;
        std::fs::write(source.join("extra.txt"), b"unexpected")?;
        assert!(verify_tree(&source, &record(), &patch).is_err());
        std::fs::remove_file(source.join("extra.txt"))?;
        std::fs::write(source.join("sample.txt"), b"changed after patch\n")?;
        assert!(verify_tree(&source, &record(), &patch).is_err());
        Ok(())
    })
}

#[test]
fn vendor_records_require_the_exact_published_identity_and_regular_relative_files(
) -> Result<(), super::VendorError> {
    let mut original = record();
    original.validate()?;
    original.version = "0.25.1".into();
    assert!(original.validate().is_err());
    original = record();
    original.archive_sha256 = "0".repeat(64);
    assert!(original.validate().is_err());
    for path in ["../outside", "/absolute", "a\\b", "a//b", "a/./b"] {
        let mut original = record();
        original.files = BTreeMap::from([(path.into(), digest(b"upstream\n"))]);
        assert!(original.validate().is_err());
    }
    #[cfg(unix)]
    fixture(|root| {
        let source = root.join("source");
        std::fs::create_dir(&source)?;
        let outside = root.join("outside");
        std::fs::write(&outside, b"upstream\n")?;
        std::os::unix::fs::symlink(outside, source.join("sample.txt"))?;
        let patch = root.join("empty.patch");
        std::fs::write(&patch, b"")?;
        assert!(verify_tree(&source, &record(), &patch).is_err());
        Ok(())
    })?;
    Ok(())
}
