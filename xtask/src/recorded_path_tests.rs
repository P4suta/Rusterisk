use super::recorded_path::RecordedPathFormat::{Unix, Windows};
use std::{io, path::Path};

#[test]
fn windows_paths_compare_without_accessing_the_recorded_volume() -> io::Result<()> {
    let workspace = Windows.parse(Path::new(r"\\?\D:\Run\Consumer\work"), 4096)?;
    let source = Windows.parse(Path::new(r"d:\run\consumer\work\src\lib.rs"), 4096)?;
    assert!(source.starts_with(&workspace));
    assert_eq!(source, workspace.join("src/lib.rs"));
    assert_eq!(source.parent(), Some(workspace.join("src")));
    assert_eq!(source.file_name(), Some("lib.rs"));
    let sibling = Windows.parse(Path::new(r"D:\Run\Consumer\work-copy"), 4096)?;
    assert!(!sibling.starts_with(&workspace));
    let foreign_volume = Windows.parse(Path::new(r"C:\Run\Consumer\work\src"), 4096)?;
    assert!(!foreign_volume.starts_with(&workspace));
    assert!(Unix
        .parse(Path::new(r"D:\Run\Consumer\work"), 4096)
        .is_err());
    Ok(())
}

#[test]
fn recorded_windows_paths_reject_ambiguous_or_unrelated_namespaces() {
    for invalid in [
        r"D:rel\file",
        r"\current-drive\file",
        r"\\host\share\file",
        r"\\?\UNC\host\share\file",
        r"\\.\D:\file",
        r"D:\parent\..\file",
        r"D:\parent\.\file",
        r"D:\parent\\file",
        r"D:\parent\file:stream",
        r"D:\parent\file.",
        r"D:\parent\file ",
        r"D:\parent\NUL.txt",
        r"D:\parent\LPT².txt",
    ] {
        assert!(
            Windows.parse(Path::new(invalid), 4096).is_err(),
            "{invalid}"
        );
    }
}

#[test]
fn recorded_unix_paths_compare_exact_components_and_case() -> io::Result<()> {
    let workspace = Unix.parse(Path::new("/run/Consumer/work"), 4096)?;
    let source = Unix.parse(Path::new("/run/Consumer/work/src/lib.rs"), 4096)?;
    assert_eq!(source, workspace.join("src/lib.rs"));
    assert_eq!(source.parent(), Some(workspace.join("src")));
    let wrong_case = Unix.parse(Path::new("/run/consumer/work/src/lib.rs"), 4096)?;
    assert!(!wrong_case.starts_with(&workspace));
    assert!(Unix.parse(Path::new("/run/../elsewhere"), 4096).is_err());
    assert!(Unix.parse(Path::new("/run/Consumer/work"), 8).is_err());
    assert!(Windows
        .parse(Path::new("/run/Consumer/work"), 4096)
        .is_err());
    Ok(())
}
