use std::path::Path;
use xtask::distribution::{check_sdk_archive_inputs, validate_sdk_archive_inputs, SdkArchiveError};

const CLIENT: &str = r#"
[package]
name = "rusterisk-client"
version.workspace = true
exclude = ["tests/**", "examples/**"]

[[example]]
name = "call"
test = true

[dependencies]
rusterisk-api = { version = "0.1.0", path = "../api" }
rusterisk-core = { version = "0.1.0", path = "../core" }

[dev-dependencies]
rusterisk-server = { path = "../server" }
rusterisk-config = { path = "../config" }
rusterisk-media = { path = "../media" }
"#;

#[test]
fn sdk_client_archive_omits_workspace_fixtures_without_changing_checkout_targets() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let result = check_sdk_archive_inputs(&root);
    assert!(result.is_ok(), "{result:?}");
}

#[test]
fn ordinary_sdk_dependencies_keep_registry_versions_and_workspace_paths() {
    let result = validate_sdk_archive_inputs(CLIENT, "0.1.0");
    assert!(result.is_ok(), "{result:?}");
    for (name, path) in [("rusterisk-api", "../api"), ("rusterisk-core", "../core")] {
        let valid = format!("{name} = {{ version = \"0.1.0\", path = \"{path}\" }}");
        for invalid in [
            format!("{name} = {{ path = \"{path}\" }}"),
            format!("{name} = {{ version = \"0.2.0\", path = \"{path}\" }}"),
            format!("{name} = {{ version = \"0.1.0\", path = \"../../other\" }}"),
        ] {
            let source = CLIENT.replace(&valid, &invalid);
            assert_ne!(source, CLIENT);
            let result = validate_sdk_archive_inputs(&source, "0.1.0");
            assert!(
                matches!(result, Err(SdkArchiveError::Dependency { name: found }) if found == name),
                "{result:?}"
            );
        }
    }
}

#[test]
fn workspace_only_dev_dependencies_cannot_leak_registry_requirements() {
    for (name, path) in [
        ("rusterisk-server", "../server"),
        ("rusterisk-config", "../config"),
        ("rusterisk-media", "../media"),
    ] {
        let valid = format!("{name} = {{ path = \"{path}\" }}");
        for invalid in [
            format!("{name} = {{ version = \"0.1.0\", path = \"{path}\" }}"),
            format!("{name} = \"0.1.0\""),
            format!("{name} = {{ path = \"../../other\" }}"),
        ] {
            let source = CLIENT.replace(&valid, &invalid);
            assert_ne!(source, CLIENT);
            let result = validate_sdk_archive_inputs(&source, "0.1.0");
            assert!(
                matches!(result, Err(SdkArchiveError::Dependency { name: found }) if found == name),
                "{result:?}"
            );
        }
    }
}

#[test]
fn archive_selection_excludes_only_workspace_tests_and_examples() {
    let valid = "exclude = [\"tests/**\", \"examples/**\"]";
    for invalid in [
        "",
        "exclude = [\"tests/**\"]",
        "exclude = [\"tests/**\", \"tests/**\"]",
        "exclude = [\"tests/**\", \"examples/**\", \"src/**\"]",
        "exclude = [\"tests/**\", \"examples/**\"]\ninclude = [\"src/**\"]",
    ] {
        let result = validate_sdk_archive_inputs(&CLIENT.replace(valid, invalid), "0.1.0");
        assert!(
            matches!(result, Err(SdkArchiveError::Exclusions)),
            "{result:?}"
        );
    }
    let reversed = CLIENT.replace(valid, "exclude = [\"examples/**\", \"tests/**\"]");
    assert!(validate_sdk_archive_inputs(&reversed, "0.1.0").is_ok());
}
