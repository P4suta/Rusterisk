use std::{collections::BTreeMap, path::Path};
use xtask::distribution::{
    check_dependency_paths, check_dependency_paths_with, validate_dependency_manifest,
    DependencyPathError, SdkInputError,
};

const ROOT: &str = "[workspace]\nmembers = ['crates/client', 'xtask']\n[workspace.dependencies]\nshared = { version = '1.0.0', path = 'shared' }\n";
const CLIENT: &str = "[package]\nname = 'rusterisk-client'\n[dev-dependencies]\nrusterisk-server = { path = '../server' }\nrusterisk-config = { path = '../config' }\nrusterisk-media = { path = '../media' }\n";

fn fixture() -> BTreeMap<String, Vec<u8>> {
    BTreeMap::from([
        ("Cargo.toml".to_owned(), ROOT.as_bytes().to_vec()),
        (
            "crates/client/Cargo.toml".to_owned(),
            CLIENT.as_bytes().to_vec(),
        ),
        (
            "xtask/Cargo.toml".to_owned(),
            b"[package]\nname = 'xtask'\npublish = false\n".to_vec(),
        ),
        (
            "xtask/fuzz/Cargo.toml".to_owned(),
            b"[workspace]\n[package]\nname = 'fuzz'\npublish = false\n".to_vec(),
        ),
        (
            "crates/core/assurance/Cargo.toml".to_owned(),
            b"[workspace]\n[package]\nname = 'assurance'\npublish = false\n".to_vec(),
        ),
        (
            "crates/core/proofs/Cargo.toml".to_owned(),
            b"[workspace]\n[package]\nname = 'proofs'\npublish = false\n".to_vec(),
        ),
    ])
}

fn check(files: &BTreeMap<String, Vec<u8>>) -> Result<usize, DependencyPathError> {
    check_dependency_paths_with(|path| {
        files
            .get(path)
            .cloned()
            .ok_or_else(|| SdkInputError::Input {
                path: path.to_owned(),
                source: std::io::Error::from(std::io::ErrorKind::NotFound),
            })
    })
}

#[test]
fn dependency_path_policy_covers_real_members_and_independent_verification_manifests() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let result = check_dependency_paths(&root);
    assert!(result.is_ok(), "{result:?}");
    assert!(matches!(check(&fixture()), Ok(6)));
}

#[test]
fn only_the_three_direct_client_development_paths_are_unversioned() {
    assert!(validate_dependency_manifest("crates/client/Cargo.toml", CLIENT, ROOT).is_ok());
    for source in [
        CLIENT.replace("[dev-dependencies]", "[dependencies]"),
        CLIENT.replace("[dev-dependencies]", "[build-dependencies]"),
        CLIENT.replace(
            "[dev-dependencies]",
            "[target.'cfg(unix)'.dev-dependencies]",
        ),
        CLIENT.replace("rusterisk-server =", "server_alias ="),
        CLIENT.replace("../server", "../other"),
        CLIENT.replace("name = 'rusterisk-client'", "name = 'other'"),
        CLIENT.replace("path = '../server'", "git = 'https://example.test/server'"),
        CLIENT.replace("path = '../server'", "path = '../server', version = '*'"),
    ] {
        assert_ne!(source, CLIENT);
        assert!(matches!(
            validate_dependency_manifest("crates/client/Cargo.toml", &source, ROOT),
            Err(DependencyPathError::Unversioned { .. })
        ));
    }
    assert!(matches!(
        validate_dependency_manifest("crates/other/Cargo.toml", CLIENT, ROOT),
        Err(DependencyPathError::Unversioned { .. })
    ));
}

#[test]
fn public_private_target_build_and_development_sources_cannot_bypass_versions() {
    for private in [false, true] {
        for table in [
            "dependencies",
            "dev-dependencies",
            "build-dependencies",
            "target.'cfg(unix)'.dependencies",
            "target.'cfg(windows)'.dev-dependencies",
            "target.'cfg(unix)'.build-dependencies",
            "workspace.dependencies",
        ] {
            for location in ["path = '../other'", "git = 'https://example.test/other'"] {
                for version in [
                    "",
                    ", version = '*'",
                    ", version = ' '",
                    ", version = 'x'",
                    ", version = 'X'",
                    ", version = '= *'",
                ] {
                    let source = format!("[package]\nname = 'other'\npublish = {}\n[{table}]\nother = {{ {location}{version} }}\n", !private);
                    let result =
                        validate_dependency_manifest("crates/other/Cargo.toml", &source, ROOT);
                    assert!(
                        matches!(result, Err(DependencyPathError::Unversioned { .. })),
                        "{table}: {result:?}"
                    );
                }
                let source = format!("[package]\nname = 'other'\n[{table}]\nother = {{ {location}, version = '1.0.0' }}\n");
                let result = validate_dependency_manifest("crates/other/Cargo.toml", &source, ROOT);
                assert!(result.is_ok(), "{table}: {result:?}");
            }
        }
    }
}

#[test]
fn workspace_inheritance_uses_the_authoritative_version_and_rejects_missing_sources() {
    for table in [
        "dependencies",
        "dev-dependencies",
        "build-dependencies",
        "target.'cfg(unix)'.dependencies",
    ] {
        let source = format!("[package]\nname = 'member'\n[{table}]\nshared = {{ workspace = true, features = ['test'] }}\n");
        assert!(validate_dependency_manifest("crates/member/Cargo.toml", &source, ROOT).is_ok());
        let invalid_root = ROOT.replace("version = '1.0.0', ", "");
        assert!(matches!(
            validate_dependency_manifest("crates/member/Cargo.toml", &source, &invalid_root),
            Err(DependencyPathError::Unversioned { .. })
        ));
        let missing = source.replace("shared =", "missing =");
        assert!(matches!(
            validate_dependency_manifest("crates/member/Cargo.toml", &missing, ROOT),
            Err(DependencyPathError::Inheritance { .. })
        ));
    }
}

#[test]
fn independent_fuzz_and_proof_manifests_cannot_hide_private_unversioned_paths() {
    for manifest in [
        "xtask/fuzz/Cargo.toml",
        "crates/core/assurance/Cargo.toml",
        "crates/core/proofs/Cargo.toml",
    ] {
        let mut files = fixture();
        files.insert(manifest.to_owned(), b"[workspace]\n[package]\nname = 'private'\npublish = false\n[dependencies]\nprivate_dep = { path = '../other' }\n".to_vec());
        let result = check(&files);
        assert!(
            matches!(result, Err(DependencyPathError::Unversioned { manifest: found, .. }) if found == manifest)
        );
    }
}

#[test]
fn cargo_deny_keeps_registry_wildcards_denied_with_the_path_policy() -> Result<(), toml::de::Error>
{
    let config: toml::Value = toml::from_str(include_str!("../../deny.toml"))?;
    let bans = config.get("bans");
    assert_eq!(
        bans.and_then(|value| value.get("wildcards"))
            .and_then(toml::Value::as_str),
        Some("deny")
    );
    assert_eq!(
        bans.and_then(|value| value.get("allow-wildcard-paths"))
            .and_then(toml::Value::as_bool),
        Some(true)
    );
    Ok(())
}
