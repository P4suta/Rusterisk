use std::{
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};
use xtask::browser::SourceSnapshot;

#[path = "browser_snapshot/shared_adapter.rs"]
mod shared_adapter;
#[path = "browser_snapshot/vendor.rs"]
mod vendor;

fn write(root: &Path, path: &str, bytes: &[u8]) -> Result<(), std::io::Error> {
    let path = root.join(path);
    std::fs::create_dir_all(
        path.parent()
            .ok_or_else(|| std::io::Error::other("missing fixture parent"))?,
    )?;
    std::fs::write(path, bytes)
}

fn project(root: &Path) -> Result<(), std::io::Error> {
    write(root, "Cargo.toml", b"[workspace]\nmembers=['xtask']\n")?;
    for path in [
        "Cargo.lock",
        "xtask/Cargo.toml",
        "xtask/src/lib.rs",
        "xtask/tests/contract.rs",
        "xtask/fuzz/Cargo.toml",
        "xtask/fuzz/Cargo.lock",
        "xtask/fuzz/src/targets.rs",
        "xtask/examples/example.rs",
    ] {
        write(root, path, b"fixture")?;
    }
    for path in [
        "package.json",
        "package-lock.json",
        "tsconfig.json",
        "eslint.config.mjs",
        "biome.json",
        "playwright.config.ts",
        "index.html",
        "style.css",
        "src/main.ts",
        "tests/call.spec.ts",
        "config/http.conf",
        "config/ari.conf",
        "config/pjsip.conf",
        "config/rtp.conf",
        "config/extensions.conf",
    ] {
        write(root, &format!("examples/browser/{path}"), b"fixture")?;
    }
    for file in ["package.json", "index.mjs", "index.ts"] {
        write(
            root,
            &format!("tests/interop/node-ari-client/project-service-only/{file}"),
            b"fixture",
        )?;
    }
    Ok(())
}

#[test]
fn browser_snapshot_copies_owned_inputs_and_refuses_changed_sources_and_test_additions(
) -> Result<(), std::io::Error> {
    let identity = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(std::io::Error::other)?
        .as_nanos();
    let directory = std::env::temp_dir().join(format!(
        "rusterisk-browser-snapshot-{}-{identity}",
        std::process::id()
    ));
    std::fs::create_dir(&directory)?;
    let result = (|| {
        let root = directory.join("root");
        project(&root)?;
        let source = directory.join("source");
        let state = directory.join("state");
        let snapshot = SourceSnapshot::read(&root).map_err(std::io::Error::other)?;
        assert_eq!(snapshot.digest().len(), 64);
        snapshot
            .write(&source, &state)
            .map_err(std::io::Error::other)?;
        snapshot
            .check_original(&root)
            .map_err(std::io::Error::other)?;
        snapshot
            .check_browser(&state)
            .map_err(std::io::Error::other)?;
        assert_eq!(
            std::fs::read(source.join("xtask/examples/example.rs"))?,
            b"fixture"
        );
        assert_eq!(
            std::fs::read(source.join("examples/browser/config/pjsip.conf"))?,
            b"fixture"
        );
        assert_eq!(
            std::fs::read(state.join("browser/config/pjsip.conf"))?,
            b"fixture"
        );
        write(&root, "xtask/src/lib.rs", b"changed")?;
        assert!(snapshot.check_original(&root).is_err());
        write(&root, "xtask/src/lib.rs", b"fixture")?;
        write(&state, "browser/tests/hidden.spec.ts", b"fixture")?;
        assert!(snapshot.check_browser(&state).is_err());
        std::fs::remove_file(state.join("browser/tests/hidden.spec.ts"))?;
        write(&state, "browser/src/main.ts", b"changed")?;
        assert!(snapshot.check_browser(&state).is_err());
        write(
            &root,
            "examples/browser/src/main.ts",
            &vec![b'x'; 1024 * 1024 + 1],
        )?;
        assert!(SourceSnapshot::read(&root).is_err());
        write(&root, "examples/browser/src/main.ts", b"fixture")?;
        assert!(SourceSnapshot::read(&root).is_ok());
        #[cfg(unix)]
        {
            let outside = directory.join("outside");
            std::fs::rename(root.join("examples"), &outside)?;
            std::os::unix::fs::symlink(&outside, root.join("examples"))?;
            assert!(
                SourceSnapshot::read(&root).is_err(),
                "Source ancestors must not redirect outside the snapshot root."
            );
        }
        Ok(())
    })();
    let cleanup = std::fs::remove_dir_all(directory);
    match (result, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(primary), Err(cleanup)) => Err(std::io::Error::other(format!(
            "{primary}; fixture cleanup failed: {cleanup}"
        ))),
    }
}
