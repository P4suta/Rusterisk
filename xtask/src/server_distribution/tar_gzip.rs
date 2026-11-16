use super::{ArchiveFormat, ServerPayload};
use std::io::{self, Write};

struct Bounded<W> {
    inner: W,
    remaining: u64,
}

impl<W: Write> Write for Bounded<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() as u64 > self.remaining {
            return Err(io::Error::other(
                "server archive exceeds its compressed size limit",
            ));
        }
        let written = self.inner.write(bytes)?;
        self.remaining -= written as u64;
        Ok(written)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

pub fn write_server_tar_gzip(payload: &ServerPayload, writer: impl Write) -> io::Result<()> {
    if payload.name().target().format() != ArchiveFormat::TarGzip {
        return Err(io::Error::other("server target requires a zip archive"));
    }
    let writer = Bounded {
        inner: writer,
        remaining: 272 * 1024 * 1024,
    };
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

#[cfg(test)]
mod tests {
    use super::Bounded;
    use std::io::Write;

    #[test]
    fn output_limits_refuse_extra_bytes_before_writing_them() -> std::io::Result<()> {
        let mut bytes = Vec::new();
        let mut writer = Bounded {
            inner: &mut bytes,
            remaining: 8,
        };
        writer.write_all(b"first")?;
        assert!(writer.write_all(b"more").is_err());
        assert_eq!(bytes, b"first");
        Ok(())
    }
}
