use super::{
    check_server_tar_gzip, check_server_zip, zip_read, ArchiveFormat, CapturedServer,
    ServerCandidateName, ServerPayload, ServerTarget,
};
use crate::regular_archive::{read_regular_gzip, ArchiveLimits, ArchiveRoot};
use sha2::{Digest, Sha256};
use std::{
    io::{self, Cursor, Read},
    path::Path,
};

fn invalid() -> io::Error {
    io::Error::other("server candidate bytes or inventory changed")
}

fn read(root: &Path, name: &str, limit: u64) -> io::Result<Vec<u8>> {
    let path = root.join(name);
    let metadata = std::fs::symlink_metadata(&path)?;
    if !metadata.is_file() || metadata.len() > limit {
        return Err(invalid());
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(limit + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(invalid());
    }
    Ok(bytes)
}

pub struct ServerCandidate {
    payload: ServerPayload,
    source_digest: String,
    checksum: String,
}

impl ServerCandidate {
    pub fn read(root: &Path, target: ServerTarget) -> io::Result<Self> {
        Self::read_paired(root, target).map(|(_, candidate)| candidate)
    }

    pub(crate) fn read_paired(
        root: &Path,
        target: ServerTarget,
    ) -> io::Result<(CapturedServer, Self)> {
        if !std::fs::symlink_metadata(root)?.is_dir() {
            return Err(invalid());
        }
        let capture = CapturedServer::read(&root.join("source"), target)?;
        let source_digest = capture.source_digest();
        let expected = [
            "source",
            "captured-browser",
            "SOURCE.sha256",
            "SHA256SUMS",
            "fetch.txt",
            "build.txt",
            capture.name().file_name(),
        ];
        let mut count = 0;
        for entry in std::fs::read_dir(root)? {
            count += 1;
            let entry = entry?;
            if count > expected.len() || !expected.iter().any(|name| entry.file_name() == *name) {
                return Err(invalid());
            }
            let directory =
                entry.file_name() == "source" || entry.file_name() == "captured-browser";
            let kind = entry.file_type()?;
            if (directory && !kind.is_dir()) || (!directory && !kind.is_file()) {
                return Err(invalid());
            }
        }
        if count != expected.len()
            || read(root, "SOURCE.sha256", 65)? != format!("{source_digest}\n").as_bytes()
        {
            return Err(invalid());
        }
        for log in ["fetch.txt", "build.txt"] {
            read(root, log, 4 * 1024 * 1024)?;
        }
        let bytes = read(root, capture.name().file_name(), 272 * 1024 * 1024)?;
        let digest: String = Sha256::digest(&bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let sums = format!("{digest}  {}\n", capture.name().file_name());
        if read(root, "SHA256SUMS", 512)? != sums.as_bytes() {
            return Err(invalid());
        }
        let (binary, cli) = match target.format() {
            ArchiveFormat::TarGzip => {
                let archive_root =
                    ArchiveRoot::new(capture.name().root()).map_err(io::Error::other)?;
                let archive =
                    read_regular_gzip(Cursor::new(&bytes), &archive_root, ArchiveLimits::SERVER)
                        .map_err(io::Error::other)?;
                let extract = |path: &str| {
                    archive
                        .files()
                        .iter()
                        .find(|file| file.path().as_str() == path)
                        .map(|file| file.bytes().to_vec())
                        .ok_or_else(invalid)
                };
                (extract(target.binary_path())?, extract(target.cli_path())?)
            }
            ArchiveFormat::Zip => zip_read::executables(capture.name(), &bytes)?,
        };
        let payload = ServerPayload::capture(target, binary, cli, |path| {
            capture
                .files()
                .find(|(file, _)| *file == Path::new(path))
                .map(|(_, bytes)| bytes.to_vec())
                .ok_or_else(invalid)
        })
        .map_err(io::Error::other)?;
        match target.format() {
            ArchiveFormat::TarGzip => check_server_tar_gzip(&payload, &bytes)?,
            ArchiveFormat::Zip => check_server_zip(&payload, &bytes)?,
        }
        capture.check(&root.join("source"))?;
        Ok((
            capture,
            Self {
                payload,
                source_digest,
                checksum: digest,
            },
        ))
    }

    pub fn name(&self) -> &ServerCandidateName {
        self.payload.name()
    }
    pub fn source_digest(&self) -> &str {
        &self.source_digest
    }
    pub fn checksum(&self) -> &str {
        &self.checksum
    }
    pub fn files(&self) -> impl Iterator<Item = (&str, &[u8])> {
        self.payload.files()
    }
}
