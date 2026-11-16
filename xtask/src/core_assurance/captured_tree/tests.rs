use super::{AssuranceTreeError, CapturedAssuranceTree};
use crate::core_assurance::{CapturedAssurance, FieldMutation};
use std::{
    fs, io,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

struct Fixture {
    project: PathBuf,
    original: PathBuf,
    working: PathBuf,
}

fn using_fixture(test: impl FnOnce(Fixture) -> io::Result<()>) -> io::Result<()> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "rusterisk-assurance-tree-fixture-{}-{stamp}",
        std::process::id()
    ));
    fs::create_dir(&root)?;
    let operation = (|| {
        let project = root.join("project");
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../crates/core");
        let snapshot = CapturedAssurance::read(&source).map_err(io::Error::other)?;
        for (path, bytes) in snapshot.inputs() {
            let path = project.join("crates/core").join(path);
            fs::create_dir_all(
                path.parent()
                    .ok_or_else(|| io::Error::other("fixture parent missing"))?,
            )?;
            fs::write(path, bytes)?;
        }
        test(Fixture {
            project,
            original: root.join("original"),
            working: root.join("working"),
        })
    })();
    match (operation, fs::remove_dir_all(root)) {
        (result, Ok(())) => result,
        (Ok(()), Err(error)) => Err(error),
        (Err(operation), Err(cleanup)) => Err(io::Error::other(format!(
            "{operation}; assurance tree fixture cleanup failed: {cleanup}"
        ))),
    }
}

#[test]
fn shared_tree_preserves_original_and_normalized_authoritative_inputs() -> io::Result<()> {
    using_fixture(|fixture| {
        let source = CapturedAssurance::read(&fixture.project.join("crates/core"))
            .map_err(io::Error::other)?;
        let tree = CapturedAssuranceTree::read(&fixture.project).map_err(io::Error::other)?;
        assert_eq!(tree.targets(), source.targets());
        assert_eq!(tree.tests(), source.tests());
        for ((project, actual), (relative, expected)) in tree.inputs().zip(source.inputs()) {
            assert_eq!(project, Path::new("crates/core").join(relative));
            assert_eq!(actual, expected);
        }
        tree.write(&fixture.original, &fixture.working)
            .map_err(io::Error::other)?;
        assert_eq!(
            fs::read(fixture.working.join("Cargo.toml"))?,
            source.normalized().as_bytes()
        );
        for (path, bytes) in source.inputs() {
            assert_eq!(
                fs::read(fixture.original.join("crates/core").join(path))?,
                bytes
            );
            if path.starts_with("src") || path.starts_with("tests") {
                assert_eq!(fs::read(fixture.working.join(path))?, bytes);
            } else if path == Path::new("assurance/Cargo.lock") {
                assert_eq!(fs::read(fixture.working.join("Cargo.lock"))?, bytes);
            }
        }
        assert!(!fixture.working.join("assurance").exists());
        fs::write(fixture.project.join("crates/core/src/lib.rs"), b"live edit")?;
        tree.check(&fixture.original, &fixture.working)
            .map_err(io::Error::other)
    })
}

#[test]
fn shared_tree_rejects_changed_original_captured_bytes() -> io::Result<()> {
    for relative in ["src/lib.rs", "Cargo.toml", "assurance/Cargo.lock"] {
        using_fixture(|fixture| {
            let tree = CapturedAssuranceTree::read(&fixture.project).map_err(io::Error::other)?;
            tree.write(&fixture.original, &fixture.working)
                .map_err(io::Error::other)?;
            let path = fixture.original.join("crates/core").join(relative);
            let mut bytes = fs::read(&path)?;
            bytes.push(b'\n');
            fs::write(path, bytes)?;
            assert!(matches!(
                tree.check(&fixture.original, &fixture.working),
                Err(AssuranceTreeError::Changed)
            ));
            Ok(())
        })?;
    }
    Ok(())
}

#[test]
fn shared_tree_rejects_changed_missing_or_additional_working_inputs() -> io::Result<()> {
    for relative in ["src/lib.rs", "Cargo.toml", "Cargo.lock"] {
        for remove in [false, true] {
            using_fixture(|fixture| {
                let tree =
                    CapturedAssuranceTree::read(&fixture.project).map_err(io::Error::other)?;
                tree.write(&fixture.original, &fixture.working)
                    .map_err(io::Error::other)?;
                let path = fixture.working.join(relative);
                if remove {
                    fs::remove_file(path)?;
                } else {
                    fs::write(path, b"changed")?;
                }
                assert!(matches!(
                    tree.check(&fixture.original, &fixture.working),
                    Err(AssuranceTreeError::Changed)
                ));
                Ok(())
            })?;
        }
    }
    for directory in [false, true] {
        using_fixture(|fixture| {
            let tree = CapturedAssuranceTree::read(&fixture.project).map_err(io::Error::other)?;
            tree.write(&fixture.original, &fixture.working)
                .map_err(io::Error::other)?;
            let path = fixture.working.join("unexpected");
            if directory {
                fs::create_dir(path)?;
            } else {
                fs::write(path, b"unexpected")?;
            }
            assert!(matches!(
                tree.check(&fixture.original, &fixture.working),
                Err(AssuranceTreeError::Changed)
            ));
            Ok(())
        })?;
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn shared_tree_rejects_linked_working_files_directories_and_input_roots() -> io::Result<()> {
    for relative in ["src/lib.rs", "src"] {
        using_fixture(|fixture| {
            let tree = CapturedAssuranceTree::read(&fixture.project).map_err(io::Error::other)?;
            tree.write(&fixture.original, &fixture.working)
                .map_err(io::Error::other)?;
            let path = fixture.working.join(relative);
            if path.is_dir() {
                fs::remove_dir_all(&path)?;
            } else {
                fs::remove_file(&path)?;
            }
            std::os::unix::fs::symlink(fixture.project.join("crates/core").join(relative), path)?;
            assert!(matches!(
                tree.check(&fixture.original, &fixture.working),
                Err(AssuranceTreeError::Changed)
            ));
            Ok(())
        })?;
    }
    using_fixture(|fixture| {
        let linked = fixture.original;
        std::os::unix::fs::symlink(fixture.project, &linked)?;
        assert!(matches!(
            CapturedAssuranceTree::read(&linked),
            Err(AssuranceTreeError::Changed)
        ));
        Ok(())
    })
}

#[test]
fn field_trees_change_only_the_authoritative_selected_assignment() -> io::Result<()> {
    for mutation in FieldMutation::ALL {
        using_fixture(|fixture| {
            let tree = CapturedAssuranceTree::read(&fixture.project).map_err(io::Error::other)?;
            tree.write(&fixture.original, &fixture.working)
                .map_err(io::Error::other)?;
            let field = fixture.working.with_file_name(mutation.label());
            tree.write_field(&field, mutation)
                .map_err(io::Error::other)?;
            let source = CapturedAssurance::read(&fixture.project.join("crates/core"))
                .map_err(io::Error::other)?;
            for (path, bytes) in source.inputs() {
                if path == Path::new("src/lib.rs") {
                    let original = std::str::from_utf8(bytes).map_err(io::Error::other)?;
                    assert_eq!(
                        fs::read(field.join(path))?,
                        mutation
                            .apply(original)
                            .map_err(io::Error::other)?
                            .as_bytes()
                    );
                } else if path.starts_with("src") || path.starts_with("tests") {
                    assert_eq!(fs::read(field.join(path))?, bytes);
                }
            }
            for path in ["Cargo.toml", "Cargo.lock"] {
                assert_eq!(
                    fs::read(field.join(path))?,
                    fs::read(fixture.working.join(path))?
                );
            }
            tree.check_field(&fixture.original, &field, mutation)
                .map_err(io::Error::other)?;
            assert!(tree.check(&fixture.original, &field).is_err());
            for other in FieldMutation::ALL {
                if other != mutation {
                    assert!(tree.check_field(&fixture.original, &field, other).is_err());
                }
            }
            fs::write(field.join("Cargo.toml"), b"changed")?;
            assert!(tree
                .check_field(&fixture.original, &field, mutation)
                .is_err());
            fs::write(field.join("Cargo.toml"), source.normalized())?;
            let path = field.join("src/unexpected.rs");
            fs::write(&path, b" ")?;
            assert!(tree
                .check_field(&fixture.original, &field, mutation)
                .is_err());
            fs::remove_file(path)?;
            fs::remove_file(field.join("Cargo.lock"))?;
            assert!(tree
                .check_field(&fixture.original, &field, mutation)
                .is_err());
            Ok(())
        })?;
    }
    Ok(())
}

#[test]
fn field_trees_reject_selector_drift_before_writing_a_variant() -> io::Result<()> {
    for mutation in FieldMutation::ALL {
        using_fixture(|fixture| {
            let path = fixture.project.join("crates/core/src/lib.rs");
            let original = fs::read_to_string(&path)?;
            fs::write(&path, original.replace(mutation.original(), ""))?;
            let tree = CapturedAssuranceTree::read(&fixture.project).map_err(io::Error::other)?;
            let field = fixture.working;
            assert!(tree.write_field(&field, mutation).is_err());
            assert!(!field.exists());
            Ok(())
        })?;
    }
    Ok(())
}
