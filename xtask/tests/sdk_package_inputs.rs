use std::{collections::BTreeMap, path::Path};
use xtask::distribution::{check_sdk_inputs_with, SdkInputError, SdkPackage};

const MANIFEST: &str = r#"
[package]
name = "sdk-fixture"
version = "0.1.0"
license.workspace = true
repository.workspace = true
readme = "../../docs/rust-sdk.md"
description = "Typed SDK package"
"#;

fn fixture() -> BTreeMap<String, Vec<u8>> {
    let mut files = BTreeMap::from([
        (
            "Cargo.toml".to_owned(),
            b"[workspace.package]\nlicense = 'MIT OR Apache-2.0'\nrepository = 'https://github.com/P4suta/Rusterisk'\n".to_vec(),
        ),
        ("LICENSE-MIT".to_owned(), b"MIT text\n".to_vec()),
        ("LICENSE-APACHE".to_owned(), b"Apache text\n".to_vec()),
        ("docs/rust-sdk.md".to_owned(), b"# Rust SDK\n".to_vec()),
    ]);
    for package in SdkPackage::ALL {
        files.insert(
            format!("{}/Cargo.toml", package.directory()),
            MANIFEST.as_bytes().to_vec(),
        );
        files.insert(
            format!("{}/LICENSE-MIT", package.directory()),
            b"MIT text\n".to_vec(),
        );
        files.insert(
            format!("{}/LICENSE-APACHE", package.directory()),
            b"Apache text\n".to_vec(),
        );
    }
    files
}

fn check(files: &BTreeMap<String, Vec<u8>>) -> Result<(), SdkInputError> {
    check_sdk_inputs_with(|path| {
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
fn sdk_packages_include_exact_licenses_and_package_documentation() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let result = xtask::distribution::check_sdk_inputs(&root);
    assert!(result.is_ok(), "{result:?}");
}

#[test]
fn valid_package_inputs_preserve_the_three_sdk_packages() {
    assert_eq!(SdkPackage::ALL.len(), 3);
    let result = check(&fixture());
    assert!(result.is_ok(), "{result:?}");
}

#[test]
fn every_package_requires_its_readme_license_repository_and_description() {
    for package in SdkPackage::ALL {
        for (field, declaration) in [
            ("readme", "readme = \"../../docs/rust-sdk.md\"\n"),
            ("license", "license.workspace = true\n"),
            ("repository", "repository.workspace = true\n"),
            ("description", "description = \"Typed SDK package\"\n"),
        ] {
            let manifest = MANIFEST.replace(declaration, "");
            assert_ne!(manifest, MANIFEST);
            let mut files = fixture();
            files.insert(
                format!("{}/Cargo.toml", package.directory()),
                manifest.into_bytes(),
            );
            let result = check(&files);
            assert!(
                matches!(result, Err(SdkInputError::Metadata { package: found, field: missing }) if found == package && missing == field),
                "{result:?}"
            );
        }
    }
}

#[test]
fn every_license_copy_must_match_the_root_bytes() {
    for package in SdkPackage::ALL {
        for file in ["LICENSE-MIT", "LICENSE-APACHE"] {
            let mut files = fixture();
            files.insert(
                format!("{}/{file}", package.directory()),
                b"changed text\n".to_vec(),
            );
            let result = check(&files);
            assert!(
                matches!(result, Err(SdkInputError::License { package: found, file: different }) if found == package && different == file),
                "{result:?}"
            );
        }
    }
}

#[test]
fn missing_or_empty_documentation_cannot_pass() {
    let mut files = fixture();
    files.remove("docs/rust-sdk.md");
    assert!(
        matches!(check(&files), Err(SdkInputError::Input { path, .. }) if path == "docs/rust-sdk.md")
    );
    files.insert("docs/rust-sdk.md".to_owned(), b" \n".to_vec());
    assert!(matches!(check(&files), Err(SdkInputError::Documentation)));
}

#[test]
fn invalid_encoding_and_oversize_inputs_are_refused() {
    for (path, bytes) in [
        ("crates/core/Cargo.toml", [0xff, 0xfe].to_vec()),
        ("docs/rust-sdk.md", [0xff, 0xfe].to_vec()),
    ] {
        let mut files = fixture();
        files.insert(path.to_owned(), bytes);
        assert!(
            matches!(check(&files), Err(SdkInputError::Encoding { path: invalid }) if invalid == path)
        );
    }
    let mut files = fixture();
    files.insert("LICENSE-MIT".to_owned(), vec![b'x'; 65537]);
    assert!(matches!(check(&files), Err(SdkInputError::Size { path }) if path == "LICENSE-MIT"));
}
