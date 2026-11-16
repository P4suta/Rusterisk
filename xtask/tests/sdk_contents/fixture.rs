use sha2::{Digest, Sha256};
use std::path::Path;
use xtask::distribution::{CapturedSdk, SdkPackage};

use crate::{archive_fixture::Fixture, captured_fixture};

pub fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn fixtures() -> Result<[Fixture; 3], std::io::Error> {
    let core = Fixture::new(SdkPackage::Core);
    let api = Fixture::new(SdkPackage::Api);
    let mut client = Fixture::new(SdkPackage::Client);
    let core_hash = digest(&core.bytes()?);
    let api_hash = digest(&api.bytes()?);
    let lock = format!("version=4\n[[package]]\nname='rusterisk-client'\nversion='0.1.0'\n[[package]]\nname='rusterisk-core'\nversion='0.1.0'\nsource='registry+https://github.com/rust-lang/crates.io-index'\nchecksum='{core_hash}'\n[[package]]\nname='rusterisk-api'\nversion='0.1.0'\nsource='registry+https://github.com/rust-lang/crates.io-index'\nchecksum='{api_hash}'\n");
    client
        .files
        .insert("Cargo.lock".to_owned(), lock.into_bytes());
    let fixtures = [core, api, client];
    for (package, fixture) in SdkPackage::ALL.iter().zip(&fixtures) {
        xtask::distribution::check_sdk_content(*package, fixture.captured(), &fixture.archive()?)
            .map_err(std::io::Error::other)?;
    }
    Ok(fixtures)
}

fn write(candidate: &Path, fixtures: &[Fixture; 3]) -> Result<(), std::io::Error> {
    let source = candidate.join("source");
    let workspace = fixtures[0]
        .source
        .get(Path::new("Cargo.toml"))
        .ok_or_else(|| std::io::Error::other("fixture workspace missing"))?;
    let workspace = std::str::from_utf8(workspace).map_err(std::io::Error::other)?;
    captured_fixture::write(&source, "Cargo.toml", format!("[workspace]\nmembers=['crates/core','crates/api','crates/client','xtask']\nresolver='2'\n{workspace}").as_bytes())?;
    let mut sums = String::new();
    for (package, fixture) in SdkPackage::ALL.iter().zip(fixtures) {
        for (path, bytes) in &fixture.source {
            if path != Path::new("Cargo.toml") {
                let relative = path
                    .to_str()
                    .ok_or_else(|| std::io::Error::other("fixture path is not UTF-8"))?;
                captured_fixture::write(&source, relative, bytes)?;
            }
        }
        let bytes = fixture.bytes()?;
        let name = format!("{}-0.1.0.crate", package.name());
        sums.push_str(&format!("{}  {name}\n", digest(&bytes)));
        std::fs::write(candidate.join(name), bytes)?;
    }
    std::fs::write(candidate.join("SHA256SUMS"), sums)?;
    let capture = CapturedSdk::read(&source).map_err(std::io::Error::other)?;
    std::fs::write(
        candidate.join("SOURCE.sha256"),
        format!("{}\n", capture.source_digest()),
    )
}

pub fn using_candidate(
    label: &str,
    prepare: impl FnOnce(&mut [Fixture; 3]) -> Result<(), std::io::Error>,
    test: impl FnOnce(&Path) -> Result<(), std::io::Error>,
) -> Result<(), std::io::Error> {
    captured_fixture::using_project(label, |source, parent| {
        let directory = std::fs::canonicalize(parent)?.join("candidate");
        std::fs::create_dir(&directory)?;
        std::fs::rename(source, directory.join("source"))?;
        let mut fixtures = fixtures()?;
        prepare(&mut fixtures)?;
        write(&directory, &fixtures)?;
        test(&directory).map_err(xtask::distribution::SdkCandidateBuildError::from)
    })
    .map_err(std::io::Error::other)
}
