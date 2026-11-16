use super::{ServerCandidateName, ServerTarget};
use crate::distribution::SourceSnapshot;
use sha2::{Digest, Sha256};
use std::{
    io::{self, Read, Write},
    path::Path,
};

const README: &str = "docs/server-distribution.md";

fn readme(root: &Path) -> io::Result<Vec<u8>> {
    let directory = std::fs::symlink_metadata(root.join("docs"))?;
    let path = root.join(README);
    let metadata = std::fs::symlink_metadata(&path)?;
    if !directory.is_dir() || !metadata.is_file() || metadata.len() > 65_536 {
        return Err(io::Error::other(
            "server startup instructions require bounded regular text",
        ));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(65_537)
        .read_to_end(&mut bytes)?;
    if bytes.is_empty()
        || bytes.len() > 65_536
        || std::str::from_utf8(&bytes).is_err()
        || bytes
            .iter()
            .any(|byte| byte.is_ascii_control() && !matches!(byte, b'\r' | b'\n' | b'\t'))
    {
        return Err(io::Error::other(
            "server startup instructions require bounded regular text",
        ));
    }
    Ok(bytes)
}

pub struct CapturedServer {
    source: SourceSnapshot,
    readme: Vec<u8>,
    name: ServerCandidateName,
}

impl CapturedServer {
    pub fn read(root: &Path, target: ServerTarget) -> io::Result<Self> {
        let source = SourceSnapshot::read(root).map_err(io::Error::other)?;
        let readme = readme(root)?;
        let manifest = source
            .files()
            .find(|(path, _)| *path == Path::new("Cargo.toml"))
            .map(|(_, bytes)| bytes)
            .ok_or_else(|| io::Error::other("server workspace manifest missing"))?;
        let manifest: toml::Value =
            toml::from_str(std::str::from_utf8(manifest).map_err(io::Error::other)?)
                .map_err(io::Error::other)?;
        let version = manifest
            .get("workspace")
            .and_then(|value| value.get("package"))
            .and_then(|value| value.get("version"))
            .and_then(toml::Value::as_str)
            .ok_or_else(|| io::Error::other("server workspace version missing"))?;
        let name = ServerCandidateName::new(version, target).map_err(io::Error::other)?;
        let captured = Self {
            source,
            readme,
            name,
        };
        captured.check(root)?;
        Ok(captured)
    }

    pub fn name(&self) -> &ServerCandidateName {
        &self.name
    }

    pub fn files(&self) -> impl Iterator<Item = (&Path, &[u8])> {
        self.source
            .files()
            .chain(std::iter::once((Path::new(README), self.readme.as_slice())))
    }

    pub fn source_digest(&self) -> String {
        let mut digest = Sha256::new();
        digest.update(self.source.digest().as_bytes());
        digest.update((README.len() as u64).to_le_bytes());
        digest.update(README.as_bytes());
        digest.update((self.readme.len() as u64).to_le_bytes());
        digest.update(&self.readme);
        digest
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    pub fn write(&self, source: &Path, state: &Path) -> io::Result<()> {
        self.source.write(source, state).map_err(io::Error::other)?;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(source.join(README))?
            .write_all(&self.readme)
    }

    pub fn check(&self, source: &Path) -> io::Result<()> {
        self.source.check(source).map_err(io::Error::other)?;
        if readme(source)? != self.readme {
            return Err(io::Error::other(
                "captured server startup instructions changed",
            ));
        }
        Ok(())
    }
}
