use super::fixture::using_candidate;
use std::path::Path;
use xtask::distribution::{SdkContents, SdkPackage};

#[test]
fn candidate_contents_are_bound_to_one_source_and_all_three_archive_checksums(
) -> Result<(), std::io::Error> {
    using_candidate(
        "valid",
        |_| Ok(()),
        |candidate| {
            let contents = SdkContents::read(candidate).map_err(std::io::Error::other)?;
            assert_eq!(contents.version(), "0.1.0");
            assert_eq!(contents.source_digest().len(), 64);
            for package in SdkPackage::ALL {
                assert_eq!(contents.checksum(package).len(), 64);
                assert!(contents
                    .archive(package)
                    .files()
                    .iter()
                    .any(|file| file.path().as_str() == "src/lib.rs"));
            }
            Ok(())
        },
    )
}

#[test]
fn stale_source_markers_checksums_and_archive_bytes_are_rejected() -> Result<(), std::io::Error> {
    for file in ["SOURCE.sha256", "SHA256SUMS", "rusterisk-core-0.1.0.crate"] {
        using_candidate(
            "stale",
            |_| Ok(()),
            |candidate| {
                std::fs::write(candidate.join(file), b"stale\n")?;
                assert!(SdkContents::read(candidate).is_err(), "{file}");
                Ok(())
            },
        )?;
    }
    Ok(())
}

#[test]
fn checksum_lists_cannot_redirect_reorder_duplicate_or_omit_candidate_names(
) -> Result<(), std::io::Error> {
    for variant in 0..5 {
        using_candidate(
            "list",
            |_| Ok(()),
            |candidate| {
                let sums = std::fs::read_to_string(candidate.join("SHA256SUMS"))?;
                let changed = match variant {
                    0 => sums.replace(
                        "rusterisk-core-0.1.0.crate",
                        "../rusterisk-core-0.1.0.crate",
                    ),
                    1 => sums.lines().rev().map(|line| format!("{line}\n")).collect(),
                    2 => format!("{sums}{sums}"),
                    3 => sums.trim_end().to_owned(),
                    _ => sums
                        .lines()
                        .skip(1)
                        .map(|line| format!("{line}\n"))
                        .collect(),
                };
                std::fs::write(candidate.join("SHA256SUMS"), changed)?;
                assert!(SdkContents::read(candidate).is_err());
                Ok(())
            },
        )?;
    }
    Ok(())
}

#[test]
fn another_crate_cannot_be_added_to_the_selected_candidate_set() -> Result<(), std::io::Error> {
    using_candidate(
        "extra",
        |_| Ok(()),
        |candidate| {
            std::fs::write(candidate.join("rusterisk-server-0.1.0.crate"), b"extra")?;
            assert!(SdkContents::read(candidate).is_err());
            Ok(())
        },
    )
}

#[test]
fn candidate_directory_inventory_has_a_fixed_upper_bound() -> Result<(), std::io::Error> {
    using_candidate(
        "entries",
        |_| Ok(()),
        |candidate| {
            for index in 0..65 {
                std::fs::write(candidate.join(format!("extra-{index}")), b"extra")?;
            }
            assert!(SdkContents::read(candidate).is_err());
            Ok(())
        },
    )
}

#[test]
fn client_lockfiles_must_bind_the_same_core_and_api_candidate_bytes() -> Result<(), std::io::Error>
{
    using_candidate(
        "cross-checksum",
        |fixtures| {
            let core = fixtures
                .get_mut(0)
                .ok_or_else(|| std::io::Error::other("core fixture missing"))?;
            let changed = b"pub const VALUE: u8 = 2;\n".to_vec();
            core.source.insert(
                Path::new("crates/core/src/lib.rs").to_owned(),
                changed.clone(),
            );
            core.files.insert("src/lib.rs".to_owned(), changed);
            Ok(())
        },
        |candidate| {
            assert!(SdkContents::read(candidate).is_err());
            Ok(())
        },
    )
}

#[test]
fn the_archive_prefix_and_manifest_must_use_the_captured_workspace_version(
) -> Result<(), std::io::Error> {
    using_candidate(
        "version",
        |fixtures| {
            let core = fixtures
                .get_mut(0)
                .ok_or_else(|| std::io::Error::other("core fixture missing"))?;
            let original = core
                .files
                .get("Cargo.toml.orig")
                .ok_or_else(|| std::io::Error::other("original fixture missing"))?;
            let original = std::str::from_utf8(original)
                .map_err(std::io::Error::other)?
                .replace("version.workspace=true", "version='0.2.0'")
                .into_bytes();
            core.source.insert(
                Path::new("crates/core/Cargo.toml").to_owned(),
                original.clone(),
            );
            core.files.insert("Cargo.toml.orig".to_owned(), original);
            for file in ["Cargo.toml", "Cargo.lock"] {
                let value = core
                    .files
                    .get(file)
                    .ok_or_else(|| std::io::Error::other("version fixture missing"))?;
                let changed = std::str::from_utf8(value)
                    .map_err(std::io::Error::other)?
                    .replace("version='0.1.0'", "version='0.2.0'");
                core.files.insert(file.to_owned(), changed.into_bytes());
            }
            Ok(())
        },
        |candidate| {
            assert!(SdkContents::read(candidate).is_err());
            Ok(())
        },
    )
}

#[cfg(unix)]
#[test]
fn candidate_files_and_root_paths_cannot_redirect_through_symlinks() -> Result<(), std::io::Error> {
    using_candidate(
        "symlink",
        |_| Ok(()),
        |candidate| {
            let archive = candidate.join("rusterisk-core-0.1.0.crate");
            let saved = candidate.join("saved-core");
            std::fs::rename(&archive, &saved)?;
            std::os::unix::fs::symlink(&saved, &archive)?;
            assert!(SdkContents::read(candidate).is_err());
            std::fs::remove_file(&archive)?;
            std::fs::rename(&saved, &archive)?;
            let alias = candidate.join("redirect");
            std::os::unix::fs::symlink(candidate, &alias)?;
            assert!(SdkContents::read(&alias).is_err());
            Ok(())
        },
    )
}
