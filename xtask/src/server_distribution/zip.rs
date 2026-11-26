use super::{output::Bounded, ArchiveFormat, ServerPayload};
use std::io::{self, Write};
use zip::{write::SimpleFileOptions, CompressionMethod, DateTime, System, ZipWriter};

pub fn write_server_zip(payload: &ServerPayload, writer: impl Write) -> io::Result<()> {
    if payload.name().target().format() != ArchiveFormat::Zip {
        return Err(io::Error::other("server target requires a tar.gz archive"));
    }
    let mut archive = ZipWriter::new_stream(Bounded::new(writer));
    let time = DateTime::from_date_and_time(1980, 1, 1, 0, 0, 0).map_err(io::Error::other)?;
    for (path, bytes) in payload.files() {
        let mode = if payload.name().target().is_executable(path) {
            0o755
        } else {
            0o644
        };
        let options = SimpleFileOptions::default()
            .compression_method(CompressionMethod::Deflated)
            .compression_level(Some(6))
            .last_modified_time(time)
            .system(System::Unix)
            .unix_permissions(mode)
            .large_file(false);
        archive.start_file(format!("{}/{path}", payload.name().root()), options)?;
        archive.write_all(bytes)?;
    }
    archive.finish()?.into_inner().flush()
}
