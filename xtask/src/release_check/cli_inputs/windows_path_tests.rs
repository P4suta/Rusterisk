use super::{resolve, ReleaseInputError};
use std::{
    io,
    path::{Path, PathBuf},
};

const BASE: &str = r"\\?\C:\release-inputs";

#[test]
fn windows_release_inputs_reject_drive_relative_paths() {
    for value in [r"C:captured-source", r"D:captured-source", r"\\?\D:captured-source"] {
        let mut path = PathBuf::from(value);
        assert!(
            matches!(resolve(&mut path, Path::new(BASE)), Err(ReleaseInputError::Path)),
            "Drive-relative Windows input was accepted: {value}"
        );
    }
}

#[test]
fn windows_release_inputs_reject_root_relative_paths() {
    for value in [r"\captured-source", "/captured-source"] {
        let mut path = PathBuf::from(value);
        assert!(
            matches!(resolve(&mut path, Path::new(BASE)), Err(ReleaseInputError::Path)),
            "Root-relative Windows input was accepted: {value}"
        );
    }
}

#[test]
fn windows_release_inputs_keep_relative_and_absolute_paths() -> io::Result<()> {
    for (value, expected) in [
        ("captured-source", r"\\?\C:\release-inputs\captured-source"),
        (r"D:\captured-source", r"D:\captured-source"),
        (r"\\?\D:\captured-source", r"\\?\D:\captured-source"),
    ] {
        let mut path = PathBuf::from(value);
        resolve(&mut path, Path::new(BASE)).map_err(io::Error::other)?;
        assert_eq!(path.as_path(), Path::new(expected));
    }
    Ok(())
}
