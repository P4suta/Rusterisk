use super::BuildDirectory;
use std::{
    io,
    time::{SystemTime, UNIX_EPOCH},
};

#[test]
fn execution_directory_collisions_preserve_existing_entries_and_remain_bounded() -> io::Result<()> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    let parent = std::fs::canonicalize(std::env::temp_dir())?.join(format!(
        "rusterisk-build-directory-fixture-{}-{stamp}",
        std::process::id()
    ));
    std::fs::create_dir(&parent)?;
    let operation = (|| {
        let first = BuildDirectory::reserve(&parent, "same-clock")?;
        let first_path = first.path()?.to_owned();
        std::fs::write(first_path.join("sentinel"), b"preserved")?;
        let second = BuildDirectory::reserve(&parent, "same-clock")?;
        let second_path = second.path()?.to_owned();
        assert_ne!(first_path, second_path);
        assert_ne!(first.identity(), second.identity());
        second.close()?;
        assert!(!second_path.exists());
        assert_eq!(std::fs::read(first_path.join("sentinel"))?, b"preserved");
        first.close()?;
        assert!(!first_path.exists());
        let dropped = BuildDirectory::reserve(&parent, "drop")?;
        let dropped_path = dropped.path()?.to_owned();
        drop(dropped);
        assert!(!dropped_path.exists());
        for sequence in 0..32 {
            let occupied = parent.join(format!("rusterisk-server-package-full-{sequence}"));
            std::fs::create_dir(&occupied)?;
            std::fs::write(occupied.join("sentinel"), b"preserved")?;
        }
        let failure = BuildDirectory::reserve(&parent, "full")
            .err()
            .ok_or_else(|| io::Error::other("exhausted directory reservations passed"))?;
        assert_eq!(failure.kind(), io::ErrorKind::AlreadyExists);
        for sequence in 0..32 {
            assert_eq!(
                std::fs::read(
                    parent.join(format!("rusterisk-server-package-full-{sequence}/sentinel"))
                )?,
                b"preserved"
            );
        }
        for identity in ["", "../escape", "-prefix", &"a".repeat(62)] {
            assert!(BuildDirectory::reserve(&parent, identity).is_err());
        }
        Ok(())
    })();
    let cleanup = std::fs::remove_dir_all(parent);
    match (operation, cleanup) {
        (operation, Ok(())) => operation,
        (Ok(()), Err(error)) => Err(error),
        (Err(operation), Err(cleanup)) => Err(io::Error::other(format!(
            "{operation}; fixture cleanup failed: {cleanup}"
        ))),
    }
}
