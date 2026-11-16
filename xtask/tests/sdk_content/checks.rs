use super::fixture::Fixture;
use xtask::distribution::{check_sdk_content, SdkPackage};

#[test]
fn archives_preserve_captured_public_files_and_omit_client_workspace_targets(
) -> Result<(), std::io::Error> {
    for package in SdkPackage::ALL {
        let fixture = Fixture::new(package);
        let archive = fixture.archive()?;
        check_sdk_content(package, fixture.captured(), &archive).map_err(std::io::Error::other)?;
    }
    Ok(())
}

#[test]
fn missing_or_changed_source_original_readme_and_license_bytes_are_rejected(
) -> Result<(), std::io::Error> {
    for path in [
        "Cargo.toml.orig",
        "src/lib.rs",
        "src/nested/module.rs",
        "LICENSE-MIT",
        "LICENSE-APACHE",
        "rust-sdk.md",
        "Cargo.toml",
        "Cargo.lock",
    ] {
        for remove in [false, true] {
            let mut fixture = Fixture::new(SdkPackage::Core);
            if remove {
                fixture.files.remove(path);
            } else {
                fixture.files.insert(path.to_owned(), b"changed".to_vec());
            }
            let archive = fixture.archive()?;
            assert!(
                check_sdk_content(SdkPackage::Core, fixture.captured(), &archive).is_err(),
                "{path}, remove={remove}"
            );
        }
    }
    Ok(())
}

#[test]
fn unexpected_files_and_excluded_client_tests_or_examples_are_rejected(
) -> Result<(), std::io::Error> {
    for path in [
        "build.rs",
        "src/injected.rs",
        "tests/contract.rs",
        "examples/workspace.rs",
        ".cargo/config.toml",
        ".cargo_vcs_info.json",
    ] {
        let mut fixture = Fixture::new(SdkPackage::Client);
        fixture
            .files
            .insert(path.to_owned(), b"unexpected".to_vec());
        let archive = fixture.archive()?;
        assert!(
            check_sdk_content(SdkPackage::Client, fixture.captured(), &archive).is_err(),
            "{path}"
        );
    }
    Ok(())
}

#[test]
fn package_license_copies_must_match_the_captured_root_originals() -> Result<(), std::io::Error> {
    let mut fixture = Fixture::new(SdkPackage::Api);
    fixture
        .source
        .insert("crates/api/LICENSE-MIT".into(), b"different".to_vec());
    fixture
        .files
        .insert("LICENSE-MIT".to_owned(), b"different".to_vec());
    let archive = fixture.archive()?;
    assert!(check_sdk_content(SdkPackage::Api, fixture.captured(), &archive).is_err());
    Ok(())
}

#[test]
fn every_public_test_target_corresponds_to_a_captured_top_level_test() -> Result<(), std::io::Error>
{
    for (valid, invalid) in [
        ("[[test]]\nname='contract'\npath='tests/contract.rs'\n", ""),
        ("name='contract'", "name='missing'"),
    ] {
        let mut fixture = Fixture::new(SdkPackage::Api);
        let manifest = fixture
            .files
            .get("Cargo.toml")
            .ok_or_else(|| std::io::Error::other("fixture manifest missing"))?;
        let text = std::str::from_utf8(manifest).map_err(std::io::Error::other)?;
        fixture.files.insert(
            "Cargo.toml".to_owned(),
            text.replace(valid, invalid).into_bytes(),
        );
        let archive = fixture.archive()?;
        assert!(check_sdk_content(SdkPackage::Api, fixture.captured(), &archive).is_err());
    }
    Ok(())
}

#[test]
fn lockfiles_require_the_exact_self_package_and_refuse_backend_dependencies(
) -> Result<(), std::io::Error> {
    for lock in [
        "version=3\n[[package]]\nname='rusterisk-core'\nversion='0.1.0'\n",
        "version=4\n[[package]]\nname='another-core'\nversion='0.1.0'\n",
        "version=4\n[[package]]\nname='rusterisk-core'\nversion='0.2.0'\n",
        "version=4\n[[package]]\nname='rusterisk-core'\nversion='0.1.0'\nsource='registry+https://github.com/rust-lang/crates.io-index'\n",
        "version=4\n[[package]]\nname='rusterisk-core'\nversion='0.1.0'\n[[package]]\nname='rusterisk-server'\nversion='0.1.0'\n",
        "version=4\n[[package]]\nname='rusterisk-core'\nversion='0.1.0'\n[[package]]\nname='rusterisk-core'\nversion='0.1.0'\n",
    ] {
        let mut fixture = Fixture::new(SdkPackage::Core);
        fixture.files.insert("Cargo.lock".to_owned(), lock.as_bytes().to_vec());
        let archive = fixture.archive()?;
        assert!(check_sdk_content(SdkPackage::Core, fixture.captured(), &archive).is_err(), "{lock}");
    }
    Ok(())
}

#[test]
fn captured_inputs_reject_duplicates_missing_roots_and_excessive_counts(
) -> Result<(), std::io::Error> {
    let fixture = Fixture::new(SdkPackage::Client);
    let archive = fixture.archive()?;
    assert!(check_sdk_content(
        SdkPackage::Client,
        fixture.captured().chain(fixture.captured()),
        &archive
    )
    .is_err());
    assert!(check_sdk_content(
        SdkPackage::Client,
        fixture
            .captured()
            .filter(|(path, _)| *path != std::path::Path::new("Cargo.toml")),
        &archive
    )
    .is_err());
    let names: Vec<_> = (0..1025)
        .map(|index| std::path::PathBuf::from(format!("extra/{index}")))
        .collect();
    let remaining = 1024 - fixture.source.len();
    let entries = || {
        names
            .iter()
            .map(|path| (path.as_path(), b"entry".as_slice()))
    };
    assert!(check_sdk_content(
        SdkPackage::Client,
        fixture.captured().chain(entries().take(remaining)),
        &archive
    )
    .is_ok());
    assert!(check_sdk_content(
        SdkPackage::Client,
        fixture.captured().chain(entries().take(remaining + 1)),
        &archive
    )
    .is_err());
    Ok(())
}

#[test]
fn captured_byte_limits_are_exact_and_paths_remain_relative() -> Result<(), std::io::Error> {
    let fixture = Fixture::new(SdkPackage::Client);
    let archive = fixture.archive()?;
    for path in ["", "../outside", "/outside", "extras/../outside"] {
        let extra = std::iter::once((std::path::Path::new(path), b"extra".as_slice()));
        assert!(check_sdk_content(
            SdkPackage::Client,
            fixture.captured().chain(extra),
            &archive
        )
        .is_err());
    }
    let large = vec![0; 8 * 1024 * 1024 + 1];
    let oversized = std::iter::once((std::path::Path::new("extra/large"), large.as_slice()));
    assert!(check_sdk_content(
        SdkPackage::Client,
        fixture.captured().chain(oversized),
        &archive
    )
    .is_err());
    let used: usize = fixture.source.values().map(Vec::len).sum();
    let remaining = (8 * 1024 * 1024usize)
        .checked_sub(used)
        .ok_or_else(|| std::io::Error::other("fixture exceeds file allowance"))?;
    let bytes = large
        .get(..8 * 1024 * 1024)
        .ok_or_else(|| std::io::Error::other("fixture large file missing"))?;
    let paths = ["extra/0", "extra/1", "extra/2", "extra/3"];
    for excess in [0, 1] {
        let last = large
            .get(..remaining + excess)
            .ok_or_else(|| std::io::Error::other("fixture final file missing"))?;
        let extras = paths
            .iter()
            .map(|path| (std::path::Path::new(path), bytes))
            .chain(std::iter::once((std::path::Path::new("extra/4"), last)));
        assert_eq!(
            check_sdk_content(
                SdkPackage::Client,
                fixture.captured().chain(extras),
                &archive
            )
            .is_ok(),
            excess == 0
        );
    }
    Ok(())
}

#[test]
fn lockfile_text_limits_and_external_sources_are_checked_before_consumption(
) -> Result<(), std::io::Error> {
    let mut fixture = Fixture::new(SdkPackage::Api);
    let canonical = "registry+https://github.com/rust-lang/crates.io-index";
    let checksum = "0".repeat(64);
    let lock = format!("version=4\n[[package]]\nname='rusterisk-api'\nversion='0.1.0'\n[[package]]\nname='serde'\nversion='1.0.228'\nsource='{canonical}'\nchecksum='{checksum}'\n");
    fixture
        .files
        .insert("Cargo.lock".to_owned(), lock.as_bytes().to_vec());
    check_sdk_content(SdkPackage::Api, fixture.captured(), &fixture.archive()?)
        .map_err(std::io::Error::other)?;
    for changed in [
        lock.replace(canonical, "git+https://example.invalid/serde"),
        lock.replace(&format!("source='{canonical}'\n"), ""),
        lock.replace(&format!("checksum='{checksum}'\n"), ""),
        lock.replace(&checksum, &"A".repeat(64)),
        lock.replace("name='serde'", "name='rusterisk-server'"),
    ] {
        fixture
            .files
            .insert("Cargo.lock".to_owned(), changed.into_bytes());
        assert!(
            check_sdk_content(SdkPackage::Api, fixture.captured(), &fixture.archive()?).is_err()
        );
    }
    let mut padded = lock.into_bytes();
    padded.resize(2 * 1024 * 1024, b' ');
    fixture
        .files
        .insert("Cargo.lock".to_owned(), padded.clone());
    check_sdk_content(SdkPackage::Api, fixture.captured(), &fixture.archive()?)
        .map_err(std::io::Error::other)?;
    padded.push(b' ');
    fixture.files.insert("Cargo.lock".to_owned(), padded);
    assert!(matches!(
        check_sdk_content(SdkPackage::Api, fixture.captured(), &fixture.archive()?),
        Err(xtask::distribution::SdkContentError::Encoding)
    ));
    Ok(())
}
