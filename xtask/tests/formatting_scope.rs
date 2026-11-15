use std::{
    path::Path,
    process::{Command, Stdio},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use xtask::process::run;

fn write(root: &Path, relative: &str, source: &str) -> Result<(), std::io::Error> {
    let path = root.join(relative);
    std::fs::create_dir_all(
        path.parent()
            .ok_or_else(|| std::io::Error::other("missing fixture parent"))?,
    )?;
    std::fs::write(path, source)
}

#[test]
fn the_required_formatter_checks_every_workspace_member_without_rewriting_published_dependencies(
) -> Result<(), std::io::Error> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(std::io::Error::other)?
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "rusterisk-format-scope-{}-{stamp}",
        std::process::id()
    ));
    std::fs::create_dir(&root)?;
    let result = (|| {
        write(
            &root,
            "Cargo.toml",
            "[workspace]\nmembers=['owned','other']\nexclude=['vendor']\nresolver='2'\n",
        )?;
        write(&root, "owned/Cargo.toml", "[package]\nname='owned'\nversion='0.1.0'\nedition='2021'\n[dependencies]\nvendor={path='../vendor'}\n")?;
        write(
            &root,
            "other/Cargo.toml",
            "[package]\nname='other'\nversion='0.1.0'\nedition='2021'\n",
        )?;
        write(
            &root,
            "vendor/Cargo.toml",
            "[package]\nname='vendor'\nversion='0.1.0'\nedition='2021'\n",
        )?;
        write(&root, "vendor/src/lib.rs", "pub fn original(){ }\n")?;
        write(&root, "owned/src/lib.rs", "pub fn owned() {}\n")?;
        write(&root, "other/src/lib.rs", "pub fn other(){ }\n")?;
        let check = || {
            run(
                Command::new("cargo")
                    .args(xtask::WORKSPACE_FORMAT_CHECK)
                    .current_dir(&root)
                    .stdout(Stdio::null())
                    .stderr(Stdio::null()),
                Duration::from_secs(20),
            )
        };
        assert!(check().is_err(), "Every owned member must be checked.");
        write(&root, "other/src/lib.rs", "pub fn other() {}\n")?;
        assert!(
            check().is_ok(),
            "The published dependency must retain its own formatting."
        );
        assert_eq!(
            std::fs::read_to_string(root.join("vendor/src/lib.rs"))?,
            "pub fn original(){ }\n"
        );
        Ok(())
    })();
    let cleanup = std::fs::remove_dir_all(&root);
    result.and(cleanup)
}
