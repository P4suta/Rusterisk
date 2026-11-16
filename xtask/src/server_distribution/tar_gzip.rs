use super::output::Bounded;
use super::{ArchiveFormat, ServerPayload};
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
        header.set_mode(if path == payload.name().target().binary_path() {
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
