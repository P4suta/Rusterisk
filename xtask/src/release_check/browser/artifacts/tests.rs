use super::{bind_prepared, CapturedReleaseSource, PreparedBrowserRun};
use std::{
    fs, io,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

const IMAGE: &str = "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

#[test]
fn native_browser_binding_rejects_a_changed_capture_after_the_source_is_restored() -> io::Result<()>
{
    let project = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or_else(|| io::Error::other("Workspace directory is missing"))?;
    let captured = CapturedReleaseSource::read(project).map_err(io::Error::other)?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    let root = fs::canonicalize(std::env::temp_dir())?.join(format!(
        "rusterisk-browser-prepared-binding-{}-{stamp}",
        std::process::id()
    ));
    fs::create_dir(&root)?;
    let operation = (|| {
        let source = root.join("source");
        captured
            .write(&source, &root.join("state"))
            .map_err(io::Error::other)?;
        let path = source.join("examples/browser/src/main.ts");
        let original = fs::read(&path)?;
        fs::write(
            &path,
            b"export const changed_browser_fixture: boolean = true;\n",
        )?;
        let changed = PreparedBrowserRun::read(&source, IMAGE).map_err(io::Error::other)?;
        fs::write(&path, &original)?;
        captured.check(&source).map_err(io::Error::other)?;
        assert!(bind_prepared(&captured, &changed).is_err());
        let matching = PreparedBrowserRun::read(&source, IMAGE).map_err(io::Error::other)?;
        bind_prepared(&captured, &matching).map_err(io::Error::other)?;
        assert_eq!(fs::read(path)?, original);
        assert!(!source.join(".cache").exists());
        Ok(())
    })();
    match (operation, fs::remove_dir_all(root)) {
        (operation, Ok(())) => operation,
        (Ok(()), Err(error)) => Err(error),
        (Err(operation), Err(cleanup)) => Err(io::Error::other(format!(
            "{operation}; browser binding fixture cleanup failed: {cleanup}"
        ))),
    }
}
