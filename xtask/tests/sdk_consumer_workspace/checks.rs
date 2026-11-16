use crate::candidate_fixture::using_candidate;
use std::{io::Write, path::Path};
use xtask::distribution::{SdkConsumerScope, SdkConsumerWorkspace, SdkContents, SdkPackage};

#[test]
fn extraction_and_consumer_manifests_use_only_candidate_packages_in_fresh_owned_roots(
) -> Result<(), std::io::Error> {
    using_candidate(
        "workspace",
        |_| Ok(()),
        |candidate| {
            let contents = SdkContents::read(candidate).map_err(std::io::Error::other)?;
            let root = candidate.with_file_name("consumer-workspace");
            let workspace = SdkConsumerWorkspace::create(&contents, &root)?;
            workspace.check_sources()?;
            for package in SdkPackage::ALL {
                let directory = workspace
                    .plan()
                    .work()
                    .join("packages")
                    .join(format!("{}-0.1.0", package.name()));
                for file in contents.archive(package).files() {
                    assert_eq!(
                        std::fs::read(directory.join(file.path().as_str()))?,
                        file.bytes()
                    );
                }
            }
            for scope in SdkConsumerScope::ALL {
                let directory = workspace.plan().directory(scope);
                let text = std::fs::read_to_string(directory.join("Cargo.toml"))?;
                let manifest: toml::Value = toml::from_str(&text).map_err(std::io::Error::other)?;
                assert!(manifest.get("workspace").is_some());
                assert_eq!(
                    manifest
                        .get("package")
                        .and_then(|value| value.get("publish"))
                        .and_then(toml::Value::as_bool),
                    Some(false)
                );
                let selected: &[SdkPackage] = match scope {
                    SdkConsumerScope::CoreMsrv => &[SdkPackage::Core],
                    SdkConsumerScope::Sdk => &SdkPackage::ALL,
                };
                let dependencies = manifest
                    .get("dependencies")
                    .and_then(toml::Value::as_table)
                    .ok_or_else(|| std::io::Error::other("consumer dependencies missing"))?;
                assert_eq!(dependencies.len(), selected.len());
                for package in selected {
                    assert_eq!(
                        dependencies
                            .get(package.name())
                            .and_then(toml::Value::as_str),
                        Some("=0.1.0")
                    );
                    assert_eq!(
                        manifest
                            .get("patch")
                            .and_then(|value| value.get("crates-io"))
                            .and_then(|value| value.get(package.name()))
                            .and_then(|value| value.get("path"))
                            .and_then(toml::Value::as_str),
                        Some(format!("../../packages/{}-0.1.0", package.name()).as_str())
                    );
                }
                let seed_package = match scope {
                    SdkConsumerScope::CoreMsrv => SdkPackage::Core,
                    SdkConsumerScope::Sdk => SdkPackage::Client,
                };
                let seed = contents
                    .archive(seed_package)
                    .files()
                    .iter()
                    .find(|file| file.path().as_str() == "Cargo.lock")
                    .ok_or_else(|| std::io::Error::other("seed lock missing"))?;
                assert_eq!(std::fs::read(directory.join("Cargo.lock"))?, seed.bytes());
            }
            workspace.close()?;
            assert!(!root.exists());
            assert!(candidate.is_dir());
            Ok(())
        },
    )
}

#[test]
fn source_mutation_and_new_configuration_are_rejected_but_consumer_locks_can_resolve(
) -> Result<(), std::io::Error> {
    using_candidate(
        "mutation",
        |_| Ok(()),
        |candidate| {
            let contents = SdkContents::read(candidate).map_err(std::io::Error::other)?;
            let root = candidate.with_file_name("consumer-workspace");
            let workspace = SdkConsumerWorkspace::create(&contents, &root)?;
            for scope in SdkConsumerScope::ALL {
                std::fs::write(
                    workspace.plan().directory(scope).join("Cargo.lock"),
                    b"version=4\n",
                )?;
            }
            workspace.check_sources()?;
            let original = workspace
                .plan()
                .work()
                .join("packages/rusterisk-core-0.1.0/src/lib.rs");
            let bytes = std::fs::read(&original)?;
            std::fs::write(&original, b"changed")?;
            assert!(workspace.check_sources().is_err());
            std::fs::write(original, bytes)?;
            let configuration = workspace.plan().work().join(".cargo");
            std::fs::create_dir(&configuration)?;
            std::fs::write(
                configuration.join("config.toml"),
                b"[build]\ntarget-dir='elsewhere'\n",
            )?;
            assert!(workspace.check_sources().is_err());
            workspace.close()?;
            Ok(())
        },
    )
}

#[test]
fn setup_failures_remove_only_the_root_created_by_the_operation() -> Result<(), std::io::Error> {
    using_candidate(
        "failure",
        |_| Ok(()),
        |candidate| {
            let contents = SdkContents::read(candidate).map_err(std::io::Error::other)?;
            let root = candidate.with_file_name("consumer-workspace");
            let mut writes = 0;
            let outcome = SdkConsumerWorkspace::create_with(&contents, &root, |path, bytes| {
                writes += 1;
                if writes == 2 {
                    return Err(std::io::Error::other("injected file write failure"));
                }
                std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(path)?
                    .write_all(bytes)
            });
            assert!(outcome.is_err());
            assert_eq!(writes, 2);
            assert!(!root.exists());
            std::fs::create_dir(&root)?;
            std::fs::write(root.join("user-data"), b"preserve")?;
            assert!(SdkConsumerWorkspace::create(&contents, &root).is_err());
            assert_eq!(std::fs::read(root.join("user-data"))?, b"preserve");
            Ok(())
        },
    )
}

#[test]
fn successful_write_ports_must_preserve_the_initial_candidate_lock_bytes(
) -> Result<(), std::io::Error> {
    let mut rejected = false;
    using_candidate(
        "seed-bytes",
        |_| Ok(()),
        |candidate| {
            let contents = SdkContents::read(candidate).map_err(std::io::Error::other)?;
            let root = candidate.with_file_name("consumer-workspace");
            let outcome = SdkConsumerWorkspace::create_with(&contents, &root, |path, bytes| {
                let seed = path.starts_with(root.join("work/consumers"))
                    && path.file_name().is_some_and(|name| name == "Cargo.lock");
                let bytes = if seed {
                    b"version=4\n".as_slice()
                } else {
                    bytes
                };
                std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(path)?
                    .write_all(bytes)
            });
            rejected = outcome.is_err();
            drop(outcome);
            Ok(())
        },
    )?;
    assert!(rejected);
    Ok(())
}

#[test]
fn the_owned_state_directory_must_remain_present() -> Result<(), std::io::Error> {
    let mut rejected = false;
    using_candidate(
        "state-directory",
        |_| Ok(()),
        |candidate| {
            let contents = SdkContents::read(candidate).map_err(std::io::Error::other)?;
            let root = candidate.with_file_name("consumer-workspace");
            let workspace = SdkConsumerWorkspace::create(&contents, &root)?;
            std::fs::remove_dir(workspace.plan().state())?;
            rejected = workspace.check_sources().is_err();
            workspace.close()?;
            Ok(())
        },
    )?;
    assert!(rejected);
    Ok(())
}

#[test]
fn legacy_cargo_configuration_cannot_be_inherited_from_a_parent_directory(
) -> Result<(), std::io::Error> {
    let mut rejected = false;
    using_candidate(
        "legacy-config",
        |_| Ok(()),
        |candidate| {
            let contents = SdkContents::read(candidate).map_err(std::io::Error::other)?;
            let parent = candidate.with_file_name("legacy-parent");
            std::fs::create_dir(&parent)?;
            std::fs::create_dir(parent.join(".cargo"))?;
            std::fs::write(
                parent.join(".cargo/config"),
                b"[build]\ntarget-dir='../checkout-target'\n",
            )?;
            let outcome = SdkConsumerWorkspace::create(&contents, &parent.join("consumer"));
            rejected = outcome.is_err();
            drop(outcome);
            Ok(())
        },
    )?;
    assert!(rejected);
    Ok(())
}

#[test]
fn consumer_roots_cannot_inherit_checkout_configuration() -> Result<(), std::io::Error> {
    using_candidate(
        "independence",
        |_| Ok(()),
        |candidate| {
            let contents = SdkContents::read(candidate).map_err(std::io::Error::other)?;
            assert!(SdkConsumerWorkspace::create(
                &contents,
                &candidate.join("source/consumer-workspace")
            )
            .is_err());
            assert!(SdkConsumerWorkspace::create(&contents, Path::new("relative")).is_err());
            Ok(())
        },
    )
}

#[cfg(unix)]
#[test]
fn symlinks_and_drop_cannot_leave_a_consumer_working_copy() -> Result<(), std::io::Error> {
    using_candidate(
        "drop",
        |_| Ok(()),
        |candidate| {
            let contents = SdkContents::read(candidate).map_err(std::io::Error::other)?;
            let root = candidate.with_file_name("consumer-workspace");
            {
                let workspace = SdkConsumerWorkspace::create(&contents, &root)?;
                let file = workspace
                    .plan()
                    .work()
                    .join("packages/rusterisk-core-0.1.0/src/lib.rs");
                std::fs::remove_file(&file)?;
                std::os::unix::fs::symlink(candidate.join("source/crates/core/src/lib.rs"), file)?;
                assert!(workspace.check_sources().is_err());
            }
            assert!(!root.exists());
            Ok(())
        },
    )
}
