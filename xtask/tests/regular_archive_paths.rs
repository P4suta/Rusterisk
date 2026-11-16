use xtask::regular_archive::{ArchivePath, ArchivePathError, ArchiveRoot};

#[test]
fn archive_paths_keep_the_expected_root_and_a_portable_relative_destination(
) -> Result<(), ArchivePathError> {
    let root = ArchiveRoot::new("sdk-0.1.0")?;
    assert_eq!(root.as_str(), "sdk-0.1.0");
    let path = ArchivePath::parse(&root, b"sdk-0.1.0/src/OwnedCall.rs", 16)?;
    assert_eq!(path.as_str(), "src/OwnedCall.rs");
    assert_eq!(path.destination_key(), "src/ownedcall.rs");
    let other = ArchivePath::parse(&root, b"sdk-0.1.0/SRC/ownedcall.rs", 16)?;
    assert_eq!(path.destination_key(), other.destination_key());
    assert_eq!(
        ArchivePath::parse(&root, b"sdk-0.1.0/.cargo_vcs_info.json", 1)?.as_str(),
        ".cargo_vcs_info.json"
    );
    Ok(())
}

#[test]
fn archive_roots_cannot_select_a_parent_directory_or_a_device() {
    for root in [
        "", ".", "..", "/sdk", "one/two", "C:\\sdk", "NUL", "con.txt", "sdk.",
    ] {
        assert!(
            matches!(ArchiveRoot::new(root), Err(ArchivePathError::Root)),
            "{root}"
        );
    }
}

#[test]
fn archive_paths_refuse_traversal_absolute_paths_and_another_candidate_root(
) -> Result<(), ArchivePathError> {
    let root = ArchiveRoot::new("sdk-0.1.0")?;
    for path in [
        "sdk-0.1.0/../outside",
        "sdk-0.1.0/./file",
        "sdk-0.1.0//file",
        "sdk-0.1.0/file/",
        "sdk-0.1.0",
        "/sdk-0.1.0/file",
        "C:/sdk-0.1.0/file",
        "sdk-0.1.0\\file",
        "sdk-0.1.0/file:stream",
    ] {
        assert!(
            ArchivePath::parse(&root, path.as_bytes(), 16).is_err(),
            "{path}"
        );
    }
    assert!(matches!(
        ArchivePath::parse(&root, b"other-0.1.0/file", 16),
        Err(ArchivePathError::UnexpectedRoot)
    ));
    Ok(())
}

#[test]
fn archive_paths_refuse_windows_aliases_unicode_and_ambiguous_endings(
) -> Result<(), ArchivePathError> {
    let root = ArchiveRoot::new("sdk-0.1.0")?;
    for leaf in [
        "CON",
        "con.txt",
        "AUX.rs",
        "nul",
        "PRN",
        "COM1",
        "lpt9.data",
        "file.",
        "file ",
        "two words",
        "é",
        "file\n",
        "file\0",
    ] {
        let path = format!("sdk-0.1.0/{leaf}");
        assert!(
            ArchivePath::parse(&root, path.as_bytes(), 16).is_err(),
            "{leaf:?}"
        );
    }
    let mut path = b"sdk-0.1.0/".to_vec();
    path.push(0xff);
    assert!(ArchivePath::parse(&root, &path, 16).is_err());
    Ok(())
}

#[test]
fn archive_path_limits_reject_excess_depth_and_length() -> Result<(), ArchivePathError> {
    let root = ArchiveRoot::new("sdk-0.1.0")?;
    assert!(ArchivePath::parse(&root, b"sdk-0.1.0/file", 1).is_ok());
    assert!(matches!(
        ArchivePath::parse(&root, b"sdk-0.1.0/src/file", 1),
        Err(ArchivePathError::Depth)
    ));
    for limit in [0, 17] {
        assert!(matches!(
            ArchivePath::parse(&root, b"sdk-0.1.0/file", limit),
            Err(ArchivePathError::Limits)
        ));
    }
    let deep = format!("sdk-0.1.0/{}", ["a"; 17].join("/"));
    assert!(matches!(
        ArchivePath::parse(&root, deep.as_bytes(), 16),
        Err(ArchivePathError::Depth)
    ));
    let long = format!("sdk-0.1.0/{}", "a".repeat(241));
    assert!(ArchivePath::parse(&root, long.as_bytes(), 16).is_err());
    Ok(())
}
