use xtask::distribution::{check_sdk_manifest, SdkPackage};

const WORKSPACE: &str = r#"
[workspace.package]
version = "0.1.0"
edition = "2021"
rust-version = "1.99"
license = "MIT OR Apache-2.0"
repository = "https://github.com/P4suta/Rusterisk"
[workspace.dependencies]
serde = { version = "1.0.228", features = ["derive"] }
tokio = { version = "1.50.0", features = ["rt-multi-thread", "macros"] }
[workspace.lints.rust]
unsafe_code = "forbid"
"#;

const ORIGINAL: &str = r#"
[package]
name = "rusterisk-client"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true
description = "Typed SDK fixture"
readme = "../../docs/rust-sdk.md"
exclude = ["tests/**", "examples/**"]
[dependencies]
rusterisk-api = { version = "0.1.0", path = "../api" }
rusterisk-core = { version = "0.1.0", path = "../core" }
serde.workspace = true
[dev-dependencies]
tokio = { workspace = true, features = ["process"] }
rusterisk-server = { path = "../server" }
rusterisk-config = { path = "../config" }
rusterisk-media = { path = "../media" }
[lints]
workspace = true
"#;

const PUBLISHED: &str = r#"
[package]
name = "rusterisk-client"
version = "0.1.0"
edition = "2021"
rust-version = "1.99"
license = "MIT OR Apache-2.0"
repository = "https://github.com/P4suta/Rusterisk"
description = "Typed SDK fixture"
readme = "rust-sdk.md"
exclude = ["tests/**", "examples/**"]
build = false
autolib = false
autobins = false
autoexamples = false
autotests = false
autobenches = false
[lib]
name = "rusterisk_client"
path = "src/lib.rs"
[dependencies.rusterisk-api]
version = "0.1.0"
[dependencies.rusterisk-core]
version = "0.1.0"
[dependencies.serde]
version = "1.0.228"
features = ["derive"]
[dev-dependencies.tokio]
version = "1.50.0"
features = ["rt-multi-thread", "macros", "process"]
[lints.rust]
unsafe_code = "forbid"
"#;

#[test]
fn normalized_sdk_manifests_preserve_metadata_and_resolve_only_registry_dependencies(
) -> Result<(), xtask::distribution::SdkManifestError> {
    check_sdk_manifest(SdkPackage::Client, ORIGINAL, WORKSPACE, PUBLISHED)
}

#[test]
fn normalized_package_metadata_cannot_redirect_builds_or_change_the_source_contract() {
    for (valid, invalid) in [
        ("name = \"rusterisk-client\"", "name = \"another-client\""),
        ("version = \"0.1.0\"", "version = \"0.2.0\""),
        ("license = \"MIT OR Apache-2.0\"", "license = \"GPL-3.0\""),
        ("readme = \"rust-sdk.md\"", "readme = \"../../README.md\""),
        ("build = false", "build = \"../build.rs\""),
        ("autobins = false", "autobins = true"),
        ("path = \"src/lib.rs\"", "path = \"../../outside.rs\""),
    ] {
        let changed = PUBLISHED.replacen(valid, invalid, 1);
        assert_ne!(changed, PUBLISHED);
        assert!(check_sdk_manifest(SdkPackage::Client, ORIGINAL, WORKSPACE, &changed).is_err());
    }
}

#[test]
fn private_sdk_fixtures_only_allow_the_packaging_registry() {
    let workspace = WORKSPACE.replace(
        "[workspace.package]",
        "[workspace.package]\npublish = false",
    );
    let original = ORIGINAL.replace("[package]", "[package]\npublish.workspace = true");
    let allowed = PUBLISHED.replace(
        "[package]",
        "[package]\npublish = ['rusterisk-sdk-fixtures']",
    );
    assert!(check_sdk_manifest(SdkPackage::Client, &original, &workspace, &allowed).is_ok());
    for unexpected in [
        PUBLISHED.to_owned(),
        allowed.replace("rusterisk-sdk-fixtures", "crates-io"),
    ] {
        assert!(
            check_sdk_manifest(SdkPackage::Client, &original, &workspace, &unexpected).is_err()
        );
    }
}

#[test]
fn normalized_dependencies_reject_backend_requirements_source_fallbacks_and_extra_features() {
    for addition in [
        "\n[dev-dependencies.rusterisk-server]\nversion='0.1.0'\n",
        "\n[dependencies.backend]\npackage='rusterisk-media'\nversion='0.1.0'\n",
        "\n[target.'cfg(unix)'.dependencies.backend]\npath='../server'\n",
    ] {
        assert!(check_sdk_manifest(
            SdkPackage::Client,
            ORIGINAL,
            WORKSPACE,
            &format!("{PUBLISHED}{addition}")
        )
        .is_err());
    }
    for invalid in [
        "version = \"0.1.0\"\npath = \"../api\"",
        "version = \"0.1.0\"\ngit = \"https://example.invalid/api\"",
        "version = \"0.1.0\"\nworkspace = true",
        "version = \"0.1.0\"\nfeatures = [\"another-feature\"]",
    ] {
        let changed = PUBLISHED.replacen(
            "[dependencies.rusterisk-api]\nversion = \"0.1.0\"",
            &format!("[dependencies.rusterisk-api]\n{invalid}"),
            1,
        );
        assert_ne!(changed, PUBLISHED);
        assert!(check_sdk_manifest(SdkPackage::Client, ORIGINAL, WORKSPACE, &changed).is_err());
    }
}

#[test]
fn normalized_manifests_reject_workspace_overrides_unknown_package_fields_and_escape_targets() {
    for addition in [
        "\n[workspace]\nmembers=[]\n",
        "\n[patch.crates-io]\nserde={path='../serde'}\n",
        "\n[[bin]]\nname='extra'\npath='src/lib.rs'\n",
        "\n[[test]]\nname='escape'\npath='../outside.rs'\n",
    ] {
        assert!(check_sdk_manifest(
            SdkPackage::Client,
            ORIGINAL,
            WORKSPACE,
            &format!("{PUBLISHED}{addition}")
        )
        .is_err());
    }
    let changed = PUBLISHED.replace("build = false", "build = false\nlinks = 'foreign-abi'");
    assert!(check_sdk_manifest(SdkPackage::Client, ORIGINAL, WORKSPACE, &changed).is_err());
}

#[test]
fn manifest_inputs_are_bounded_and_require_valid_source_inheritance() {
    assert!(check_sdk_manifest(SdkPackage::Client, "[broken", WORKSPACE, PUBLISHED).is_err());
    assert!(check_sdk_manifest(SdkPackage::Client, ORIGINAL, "", PUBLISHED).is_err());
    assert!(
        check_sdk_manifest(SdkPackage::Client, ORIGINAL, WORKSPACE, &"x".repeat(65537)).is_err()
    );
    let unresolved = ORIGINAL.replace("serde.workspace = true", "absent.workspace = true");
    assert!(check_sdk_manifest(SdkPackage::Client, &unresolved, WORKSPACE, PUBLISHED).is_err());
    assert!(check_sdk_manifest(SdkPackage::Core, ORIGINAL, WORKSPACE, PUBLISHED).is_err());
}

#[test]
fn a_matching_backend_dependency_is_rejected_even_when_the_source_manifest_requests_it() {
    let backend = "\n[dependencies.rusterisk-storage]\nversion='0.1.0'\n";
    assert!(check_sdk_manifest(
        SdkPackage::Client,
        &format!("{ORIGINAL}{backend}"),
        WORKSPACE,
        &format!("{PUBLISHED}{backend}"),
    )
    .is_err());
}

#[test]
fn dependency_defaults_and_lint_priorities_are_compared_by_their_effective_settings(
) -> Result<(), xtask::distribution::SdkManifestError> {
    let reordered = PUBLISHED
        .replace(
            "features = [\"rt-multi-thread\", \"macros\", \"process\"]",
            "features = [\"process\", \"macros\", \"rt-multi-thread\"]\noptional = false\ndefault-features = true",
        )
        .replace("unsafe_code = \"forbid\"", "unsafe_code = { level = \"forbid\", priority = 0 }");
    check_sdk_manifest(SdkPackage::Client, ORIGINAL, WORKSPACE, &reordered)?;
    for changed in [
        reordered.replace("optional = false", "optional = true"),
        reordered.replace("default-features = true", "default-features = false"),
        reordered.replace("priority = 0", "priority = 1"),
        reordered.replace("level = \"forbid\"", "level = \"allow\""),
    ] {
        assert!(check_sdk_manifest(SdkPackage::Client, ORIGINAL, WORKSPACE, &changed).is_err());
    }
    Ok(())
}

#[test]
fn edition_2021_keeps_workspace_defaults_when_a_member_requests_default_features_false() {
    let original = ORIGINAL.replace(
        "serde.workspace = true",
        "serde = { workspace = true, default-features = false }",
    );
    let disabled = PUBLISHED.replace(
        "[dependencies.serde]\nversion = \"1.0.228\"",
        "[dependencies.serde]\nversion = \"1.0.228\"\ndefault-features = false",
    );
    assert_eq!(
        (
            check_sdk_manifest(SdkPackage::Client, &original, WORKSPACE, PUBLISHED).is_ok(),
            check_sdk_manifest(SdkPackage::Client, &original, WORKSPACE, &disabled).is_err(),
        ),
        (true, true),
    );
}

#[test]
fn the_package_profile_refuses_editions_without_verified_inheritance_rules() {
    assert!(check_sdk_manifest(
        SdkPackage::Client,
        ORIGINAL,
        &WORKSPACE.replace("edition = \"2021\"", "edition = \"2024\""),
        &PUBLISHED.replace("edition = \"2021\"", "edition = \"2024\""),
    )
    .is_err());
}

#[test]
fn an_inherited_disabled_default_can_be_retained_or_explicitly_enabled(
) -> Result<(), xtask::distribution::SdkManifestError> {
    let workspace = WORKSPACE.replace(
        "serde = { version = \"1.0.228\", features = [\"derive\"] }",
        "serde = { version = \"1.0.228\", features = [\"derive\"], default-features = false }",
    );
    let disabled = PUBLISHED.replace(
        "[dependencies.serde]\nversion = \"1.0.228\"",
        "[dependencies.serde]\nversion = \"1.0.228\"\ndefault-features = false",
    );
    check_sdk_manifest(SdkPackage::Client, ORIGINAL, &workspace, &disabled)?;
    let local_false = ORIGINAL.replace(
        "serde.workspace = true",
        "serde = { workspace = true, default-features = false }",
    );
    check_sdk_manifest(SdkPackage::Client, &local_false, &workspace, &disabled)?;
    let local_true = local_false.replace("default-features = false", "default-features = true");
    check_sdk_manifest(SdkPackage::Client, &local_true, &workspace, PUBLISHED)
}
