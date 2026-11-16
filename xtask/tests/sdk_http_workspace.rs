#[path = "sdk_content/fixture.rs"]
mod archive_fixture;
#[path = "sdk_contents/fixture.rs"]
mod candidate_fixture;
#[path = "sdk_candidate_build/fixture.rs"]
mod captured_fixture;

use std::path::Path;
use xtask::distribution::{SdkConsumerScope, SdkConsumerWorkspace, SdkContents};

fn add_runtime(fixtures: &mut [archive_fixture::Fixture; 3]) -> Result<(), std::io::Error> {
    let lock = fixtures[2]
        .files
        .get_mut("Cargo.lock")
        .ok_or_else(|| std::io::Error::other("fixture client lock missing"))?;
    lock.extend_from_slice(format!("[[package]]\nname='tokio'\nversion='1.54.1'\nsource='registry+https://github.com/rust-lang/crates.io-index'\nchecksum='{}'\n", "a".repeat(64)).as_bytes());
    Ok(())
}

#[test]
fn http_workspace_uses_the_locked_runtime_without_changing_the_core_msrv_consumer(
) -> Result<(), std::io::Error> {
    candidate_fixture::using_candidate("http-workspace", add_runtime, |candidate| {
        let contents = SdkContents::read(candidate).map_err(std::io::Error::other)?;
        let root = candidate.with_file_name("consumer-http-workspace");
        let workspace = SdkConsumerWorkspace::create_http(&contents, &root)?;
        workspace.check_sources()?;
        let runtime = workspace
            .runtime()
            .ok_or_else(|| std::io::Error::other("HTTP runtime missing"))?;
        assert_eq!(runtime.version(), "1.54.1");
        for scope in SdkConsumerScope::ALL {
            let directory = workspace.plan().directory(scope);
            let text = std::fs::read_to_string(directory.join("Cargo.toml"))?;
            let manifest: toml::Value = toml::from_str(&text).map_err(std::io::Error::other)?;
            let dependencies = manifest
                .get("dependencies")
                .and_then(toml::Value::as_table)
                .ok_or_else(|| std::io::Error::other("consumer dependencies missing"))?;
            match scope {
                SdkConsumerScope::CoreMsrv => {
                    assert_eq!(dependencies.len(), 1);
                    assert!(!dependencies.contains_key("tokio"));
                    assert!(!directory.join("src/http_contract.rs").exists());
                    assert_eq!(
                        std::fs::read(directory.join("src/main.rs"))?,
                        include_bytes!("../src/distribution/consumer_sources/core_main.rs")
                    );
                }
                SdkConsumerScope::Sdk => {
                    assert_eq!(dependencies.len(), 4);
                    let tokio = dependencies
                        .get("tokio")
                        .ok_or_else(|| std::io::Error::other("runtime dependency missing"))?;
                    assert_eq!(
                        tokio.get("version").and_then(toml::Value::as_str),
                        Some("=1.54.1")
                    );
                    assert_eq!(
                        tokio.get("default-features").and_then(toml::Value::as_bool),
                        Some(false)
                    );
                    let features = tokio
                        .get("features")
                        .and_then(toml::Value::as_array)
                        .ok_or_else(|| std::io::Error::other("runtime features missing"))?;
                    assert_eq!(
                        features
                            .iter()
                            .filter_map(toml::Value::as_str)
                            .collect::<Vec<_>>(),
                        ["net", "rt", "time"]
                    );
                    assert_eq!(
                        std::fs::read(directory.join("src/main.rs"))?,
                        include_bytes!("../src/distribution/consumer_sources/http_main.rs")
                    );
                    assert_eq!(
                        std::fs::read(directory.join("src/http_contract.rs"))?,
                        include_bytes!("../src/distribution/consumer_sources/http_contract.rs")
                    );
                }
            }
        }
        workspace.close()?;
        assert!(!root.exists());
        Ok(())
    })
}

#[test]
fn a_missing_runtime_is_rejected_before_creating_an_http_workspace() -> Result<(), std::io::Error> {
    candidate_fixture::using_candidate(
        "http-no-runtime",
        |_| Ok(()),
        |candidate| {
            let contents = SdkContents::read(candidate).map_err(std::io::Error::other)?;
            let root = candidate.with_file_name("consumer-http-workspace");
            assert!(SdkConsumerWorkspace::create_http(&contents, &root).is_err());
            assert!(!root.exists());
            Ok(())
        },
    )
}

#[test]
fn http_setup_cannot_inherit_checkout_configuration() -> Result<(), std::io::Error> {
    candidate_fixture::using_candidate("http-invalid-root", add_runtime, |candidate| {
        let contents = SdkContents::read(candidate).map_err(std::io::Error::other)?;
        assert!(SdkConsumerWorkspace::create_http(&contents, Path::new("relative")).is_err());
        Ok(())
    })
}
