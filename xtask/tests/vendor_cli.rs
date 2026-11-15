use std::{
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

#[test]
fn vendor_import_refuses_an_unverified_archive_before_writing_the_vendor_tree(
) -> Result<(), std::io::Error> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(std::io::Error::other)?
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "rusterisk-vendor-cli-{}-{stamp}",
        std::process::id()
    ));
    std::fs::create_dir(&root)?;
    let result = (|| {
        let archive = root.join("unverified.crate");
        std::fs::write(&archive, b"not the published crate")?;
        for command in ["vendor-import", "vendor-patch"] {
            let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
                .args([command, "--archive"])
                .arg(&archive)
                .current_dir(&root)
                .output()?;
            assert!(!output.status.success());
            let error = String::from_utf8(output.stderr).map_err(std::io::Error::other)?;
            assert!(error.contains("pinned published identity"), "{error}");
        }
        assert!(!root.join("vendor").exists());
        Ok(())
    })();
    let cleanup = std::fs::remove_dir_all(&root);
    result.and(cleanup)
}
