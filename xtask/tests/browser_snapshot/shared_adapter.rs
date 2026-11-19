use super::{directory, project, write};
use std::{fs, io, path::Path};
use xtask::browser::SourceSnapshot;

const ADAPTER: &str = "tests/interop/node-ari-client/project-service-only";
const FILES: [&str; 3] = ["package.json", "index.mjs", "index.ts"];

fn require(valid: bool, reason: &'static str) -> io::Result<()> {
    if valid {
        Ok(())
    } else {
        Err(io::Error::other(reason))
    }
}

fn using(test: impl FnOnce(&Path, &Path, &Path) -> io::Result<()>) -> io::Result<()> {
    let parent = directory::reserve("browser-shared-adapter")?;
    let root = parent.join("root");
    let source = parent.join("source");
    let state = parent.join("state");
    let operation = project(&root).and_then(|()| test(&root, &source, &state));
    match (operation, fs::remove_dir_all(&parent)) {
        (operation, Ok(())) => operation,
        (Ok(()), Err(cleanup)) => Err(cleanup),
        (Err(operation), Err(cleanup)) => Err(io::Error::other(format!(
            "{operation}; shared adapter fixture cleanup failed: {cleanup}"
        ))),
    }
}

#[test]
fn browser_snapshot_captures_the_shared_adapter_at_its_canonical_path() -> io::Result<()> {
    using(|root, source, state| {
        let snapshot = SourceSnapshot::read(root).map_err(io::Error::other)?;
        for file in FILES {
            let path = format!("{ADAPTER}/{file}");
            require(
                snapshot
                    .files()
                    .any(|(actual, bytes)| actual == Path::new(&path) && bytes == b"fixture"),
                "Browser snapshot omitted a canonical shared adapter input",
            )?;
        }
        snapshot.write(source, state).map_err(io::Error::other)?;
        for file in FILES {
            require(
                fs::read(source.join(ADAPTER).join(file))? == b"fixture",
                "Browser snapshot relocated or changed a shared adapter input",
            )?;
        }
        snapshot.check_original(root).map_err(io::Error::other)?;
        snapshot.check_original(source).map_err(io::Error::other)?;
        snapshot.check_browser(state).map_err(io::Error::other)
    })
}

#[test]
fn browser_snapshot_rejects_original_and_cloned_shared_adapter_changes() -> io::Result<()> {
    using(|root, source, state| {
        let snapshot = SourceSnapshot::read(root).map_err(io::Error::other)?;
        snapshot.write(source, state).map_err(io::Error::other)?;
        for file in FILES {
            write(root, &format!("{ADAPTER}/{file}"), b"changed")?;
            require(
                snapshot.check_original(root).is_err(),
                "Browser snapshot missed a shared adapter change in its original inputs",
            )?;
            write(root, &format!("{ADAPTER}/{file}"), b"fixture")?;
            write(source, &format!("{ADAPTER}/{file}"), b"changed")?;
            require(
                snapshot.check_original(source).is_err(),
                "Browser snapshot missed a shared adapter change in its owned source copy",
            )?;
            write(source, &format!("{ADAPTER}/{file}"), b"fixture")?;
        }
        snapshot.check_original(root).map_err(io::Error::other)?;
        snapshot.check_original(source).map_err(io::Error::other)
    })
}

#[test]
fn browser_snapshot_rejects_missing_or_unlisted_shared_adapter_files() -> io::Result<()> {
    for file in FILES {
        using(|root, _, _| {
            fs::remove_file(root.join(ADAPTER).join(file))?;
            require(
                SourceSnapshot::read(root).is_err(),
                "Browser snapshot accepted an incomplete shared adapter",
            )
        })?;
    }
    using(|root, _, _| {
        write(root, &format!("{ADAPTER}/unlisted.mjs"), b"unlisted")?;
        require(
            SourceSnapshot::read(root).is_err(),
            "Browser snapshot ignored an unlisted shared adapter input",
        )
    })
}

#[cfg(unix)]
#[test]
fn browser_snapshot_rejects_shared_adapter_file_and_directory_links() -> io::Result<()> {
    for file in FILES {
        using(|root, outside, _| {
            let path = root.join(ADAPTER).join(file);
            fs::rename(&path, outside)?;
            std::os::unix::fs::symlink(outside, &path)?;
            require(
                SourceSnapshot::read(root).is_err(),
                "Browser snapshot accepted a linked shared adapter file",
            )
        })?;
    }
    using(|root, outside, _| {
        let path = root.join(ADAPTER);
        fs::rename(&path, outside)?;
        std::os::unix::fs::symlink(outside, &path)?;
        require(
            SourceSnapshot::read(root).is_err(),
            "Browser snapshot accepted a linked shared adapter directory",
        )
    })
}
