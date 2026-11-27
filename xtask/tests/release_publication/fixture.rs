use flate2::{write::GzEncoder, Compression};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs, io,
    path::Path,
    process::{Command, Output},
};

pub const SOURCE: &str = "0123456789012345678901234567890123456789";
pub const OTHER: &str = "1212121212121212121212121212121212121212";
pub const VERSION: &str = "0.2.0";

pub fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub fn crate_bytes(source: &str) -> io::Result<Vec<u8>> {
    crate_with_readme(source, "Remote CLI\n", Compression::fast())
}

pub fn crate_with_readme(
    source: &str,
    readme: &str,
    compression: Compression,
) -> io::Result<Vec<u8>> {
    let mut archive = tar::Builder::new(GzEncoder::new(Vec::new(), compression));
    for (name, bytes) in [
        (
            "Cargo.toml",
            "[package]\nname='rusterisk-cli'\nversion='0.2.0'\nreadme='README.md'\n".to_owned(),
        ),
        (
            ".cargo_vcs_info.json",
            format!(r#"{{"git":{{"sha1":"{source}"}},"path_in_vcs":"crates/cli"}}"#),
        ),
        ("README.md", readme.to_owned()),
    ] {
        let mut header = tar::Header::new_gnu();
        header.set_mode(0o644);
        header.set_size(bytes.len() as u64);
        header.set_cksum();
        archive.append_data(
            &mut header,
            format!("rusterisk-cli-0.2.0/{name}"),
            bytes.as_bytes(),
        )?;
    }
    archive.into_inner()?.finish()
}

pub fn files(source: &str) -> io::Result<BTreeMap<String, Vec<u8>>> {
    let mut files = BTreeMap::new();
    for (triple, extension) in [
        ("aarch64-apple-darwin", "tar.gz"),
        ("x86_64-apple-darwin", "tar.gz"),
        ("x86_64-pc-windows-msvc", "zip"),
        ("aarch64-unknown-linux-gnu", "tar.gz"),
        ("x86_64-unknown-linux-gnu", "tar.gz"),
    ] {
        let server = format!("rusterisk-0.2.0-{triple}");
        files.insert(format!("{server}.{extension}"), b"archive".to_vec());
        files.insert(format!("sbom/{server}.cdx.xml"), b"server SBOM".to_vec());
        files.insert(
            format!("sbom/rusterisk-cli-0.2.0-{triple}.cdx.xml"),
            b"CLI SBOM".to_vec(),
        );
        if triple.ends_with("-linux-gnu") {
            files.insert(format!("{server}.oci.tar"), b"image".to_vec());
            files.insert(format!("sbom/{server}.oci.cdx.xml"), b"image SBOM".to_vec());
        }
    }
    files.insert("rusterisk-cli-0.2.0.crate".into(), crate_bytes(source)?);
    Ok(files)
}

pub fn write(dir: &Path, files: &BTreeMap<String, Vec<u8>>) -> io::Result<()> {
    fs::create_dir_all(dir.join("sbom"))?;
    let mut manifest = String::new();
    for (name, bytes) in files {
        fs::write(dir.join(name), bytes)?;
        manifest.push_str(&format!("{}  {name}\n", digest(bytes)));
    }
    fs::write(dir.join("SHA256SUMS"), manifest)
}

pub fn plan(dir: &Path, source: &str) -> io::Result<Output> {
    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["release-publication", "plan", VERSION, source])
        .arg(dir)
        .output()
}
