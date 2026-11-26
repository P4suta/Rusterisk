use super::output::Bounded;
use super::{ArchiveFormat, ServerPayload};
use crate::regular_archive::{read_regular_gzip, ArchiveLimits, ArchiveRoot};
use std::io::{self, Write};

pub fn write_server_tar_gzip(payload: &ServerPayload, writer: impl Write) -> io::Result<()> {
    if payload.name().target().format() != ArchiveFormat::TarGzip {
        return Err(io::Error::other("server target requires a zip archive"));
    }
    let writer = Bounded::new(writer);
    let encoder = flate2::GzBuilder::new()
        .mtime(0)
        .operating_system(255)
        .write(writer, flate2::Compression::default());
    let mut archive = tar::Builder::new(encoder);
    for (path, bytes) in payload.files() {
        let mut header = tar::Header::new_ustar();
        header.set_path(format!("{}/{path}", payload.name().root()))?;
        header.set_entry_type(tar::EntryType::Regular);
        header.set_mode(if payload.name().target().is_executable(path) {
            0o755
        } else {
            0o644
        });
        header.set_size(bytes.len() as u64);
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(0);
        header.set_cksum();
        archive.append(&header, bytes)?;
    }
    archive.into_inner()?.finish()?.flush()
}

pub fn check_server_tar_gzip(payload: &ServerPayload, bytes: &[u8]) -> io::Result<()> {
    if payload.name().target().format() != ArchiveFormat::TarGzip {
        return Err(io::Error::other("server target requires a zip archive"));
    }
    let gzip = flate2::bufread::GzDecoder::new(io::Cursor::new(bytes));
    let header = gzip
        .header()
        .ok_or_else(|| io::Error::other("server archive has no gzip header"))?;
    if header.mtime() != 0
        || header.operating_system() != 255
        || header.filename().is_some()
        || header.comment().is_some()
        || header.extra().is_some()
    {
        return Err(io::Error::other(
            "server archive has unexpected gzip metadata",
        ));
    }
    let root = ArchiveRoot::new(payload.name().root()).map_err(io::Error::other)?;
    let archive = read_regular_gzip(io::Cursor::new(bytes), &root, ArchiveLimits::SERVER)
        .map_err(io::Error::other)?;
    if archive.files().len() != super::SERVER_FILE_COUNT {
        return Err(io::Error::other(
            "server archive file count differs from the candidate",
        ));
    }
    for ((path, expected), file) in payload.files().zip(archive.files()) {
        let mode = if payload.name().target().is_executable(path) {
            0o755
        } else {
            0o644
        };
        if file.path().as_str() != path
            || file.bytes() != expected
            || file.mode()? != mode
            || file.uid()? != 0
            || file.gid()? != 0
            || file.mtime()? != 0
        {
            return Err(io::Error::other(
                "server archive differs from its captured payload",
            ));
        }
    }
    Ok(())
}
