use super::{hex, invalid, Release};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
};
use xtask::{
    regular_archive::{read_regular_gzip, ArchiveLimits, ArchiveRoot},
    server_distribution::{ServerCandidateName, ServerTarget},
};

pub(super) fn sha256(bytes: &[u8]) -> String {
    digest_text(&Sha256::digest(bytes))
}

fn digest_text(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn open_regular(path: &Path) -> io::Result<fs::File> {
    #[cfg(unix)]
    let file = {
        use rustix::fs::{open, Mode, OFlags};
        let flags =
            OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::NOCTTY;
        fs::File::from(open(path, flags, Mode::empty())?)
    };
    #[cfg(not(unix))]
    let file = {
        let mut options = fs::OpenOptions::new();
        options.read(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
            options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
        }
        options.open(path)?
    };
    let metadata = file.metadata()?;
    let regular = metadata.is_file();
    #[cfg(windows)]
    let regular = {
        use std::os::windows::fs::MetadataExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
        regular && metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0
    };
    if !regular {
        return Err(invalid(format!(
            "expected a regular release asset at {}",
            path.display()
        )));
    }
    Ok(file)
}

#[cfg(all(test, unix))]
#[test]
fn release_inputs_open_without_waiting_and_accept_only_regular_files() -> io::Result<()> {
    use rustix::fs::{fcntl_getfl, OFlags};
    let root = tempfile::tempdir()?;
    let path = root.path().join("asset");
    fs::write(&path, b"release")?;
    let link = root.path().join("link");
    std::os::unix::fs::symlink(&path, &link)?;
    assert!(open_regular(&link).is_err());
    assert!(open_regular(root.path()).is_err());
    let file = open_regular(&path)?;
    assert!(file.metadata()?.is_file());
    assert!(fcntl_getfl(&file)?.contains(OFlags::NONBLOCK));
    Ok(())
}

fn file_digest(path: &Path) -> io::Result<String> {
    let mut hash = Sha256::new();
    let mut file = open_regular(path)?;
    let mut buffer = [0; 8192];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(digest_text(&hash.finalize()))
}

pub(super) fn small_file(path: &Path, maximum: u64) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    open_regular(path)?
        .take(maximum + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > maximum {
        return Err(invalid("release input exceeds its size limit"));
    }
    Ok(bytes)
}

pub(super) fn names(release: &Release<'_>) -> io::Result<BTreeMap<String, String>> {
    let mut paths = BTreeMap::new();
    for target in ServerTarget::ALL {
        let name = ServerCandidateName::new(release.version, target).map_err(io::Error::other)?;
        let root = name.root();
        let cli = format!("rusterisk-cli-{}-{}", release.version, target.triple());
        paths.insert(name.file_name().into(), name.file_name().into());
        for name in [format!("{root}.cdx.xml"), format!("{cli}.cdx.xml")] {
            paths.insert(name.clone(), format!("sbom/{name}"));
        }
        if matches!(target, ServerTarget::LinuxArm64 | ServerTarget::LinuxX86_64) {
            let archive = format!("{root}.oci.tar");
            paths.insert(archive.clone(), archive);
            let name = format!("{root}.oci.cdx.xml");
            paths.insert(name.clone(), format!("sbom/{name}"));
        }
    }
    paths.insert(release.crate_name(), release.crate_name());
    Ok(paths)
}

pub(super) fn checksums(text: &str) -> io::Result<BTreeMap<String, String>> {
    let mut entries = BTreeMap::new();
    for line in text.lines() {
        let (digest, name) = line
            .split_once("  ")
            .ok_or_else(|| invalid("invalid checksum line"))?;
        if !hex(digest, 64) || entries.insert(name.into(), digest.into()).is_some() {
            return Err(invalid("invalid or duplicate asset checksum"));
        }
    }
    Ok(entries)
}

pub(super) struct Local {
    pub assets: BTreeMap<String, (PathBuf, String)>,
    pub manifest: String,
}

impl Local {
    pub fn verify(
        release: &Release<'_>,
        directory: &Path,
        snapshots: Option<&Path>,
    ) -> io::Result<Self> {
        let bytes = small_file(&directory.join("SHA256SUMS"), 16 * 1024)?;
        let entries = checksums(std::str::from_utf8(&bytes).map_err(io::Error::other)?)?;
        let paths = names(release)?;
        if entries.len() != paths.len() || !paths.values().all(|path| entries.contains_key(path)) {
            return Err(invalid(
                "asset inventory must include all verified archives and SBOMs",
            ));
        }
        let mut assets = BTreeMap::new();
        let mut manifest = String::new();
        for (name, relative) in paths {
            let original = directory.join(&relative);
            let snapshot = snapshots.map(|root| root.join(&name));
            let digest = if name == release.crate_name() {
                let archive = small_file(&original, 16 * 1024 * 1024)?;
                check_crate(release, &archive)?;
                if let Some(copy) = &snapshot {
                    fs::File::create_new(copy)?.write_all(&archive)?;
                }
                sha256(&archive)
            } else if let Some(copy) = &snapshot {
                io::copy(
                    &mut open_regular(&original)?,
                    &mut fs::File::create_new(copy)?,
                )?;
                file_digest(copy)?
            } else {
                file_digest(&original)?
            };
            if entries.get(&relative) != Some(&digest) {
                return Err(invalid(format!("checksum mismatch for {relative}")));
            }
            manifest.push_str(&format!("{digest}  {name}\n"));
            assets.insert(name, (snapshot.unwrap_or(original), digest));
        }
        Ok(Self { assets, manifest })
    }

    pub fn manifest_hash(&self) -> String {
        sha256(self.manifest.as_bytes())
    }
}

pub(super) fn check_crate(release: &Release<'_>, bytes: &[u8]) -> io::Result<String> {
    let name = format!("rusterisk-cli-{}", release.version);
    let root = ArchiveRoot::new(&name).map_err(io::Error::other)?;
    let archive = read_regular_gzip(bytes, &root, ArchiveLimits::SDK).map_err(io::Error::other)?;
    let file = |name: &str| {
        archive
            .files()
            .iter()
            .find(|f| f.path().as_str() == name)
            .map(|f| f.bytes())
            .ok_or_else(|| invalid(format!("missing CLI archive file {name}")))
    };
    let vcs: serde_json::Value = serde_json::from_slice(file(".cargo_vcs_info.json")?)?;
    let manifest: toml::Table =
        toml::from_str(std::str::from_utf8(file("Cargo.toml")?).map_err(io::Error::other)?)
            .map_err(io::Error::other)?;
    let package = manifest
        .get("package")
        .and_then(toml::Value::as_table)
        .ok_or_else(|| invalid("missing CLI package"))?;
    if vcs.pointer("/git/sha1").and_then(|v| v.as_str()) != Some(release.source)
        || !matches!(
            vcs.pointer("/git/dirty"),
            None | Some(serde_json::Value::Bool(false))
        )
        || vcs["path_in_vcs"] != "crates/cli"
        || package.get("name").and_then(toml::Value::as_str) != Some("rusterisk-cli")
        || package.get("version").and_then(toml::Value::as_str) != Some(release.version)
        || package.get("readme").and_then(toml::Value::as_str) != Some("README.md")
    {
        return Err(invalid(
            "CLI archive version or source differs from the release",
        ));
    }
    String::from_utf8(file("README.md")?.to_vec()).map_err(io::Error::other)
}
