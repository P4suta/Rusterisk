use std::io::{self, Cursor, Write};
use xtask::{
    regular_archive::{read_regular_gzip, ArchiveLimits, ArchiveRoot},
    server_distribution::{write_server_tar_gzip, ServerPayload, ServerTarget},
};

fn payload(version: &str) -> io::Result<ServerPayload> {
    payload_with_binary_size(version, 32)
}

fn payload_with_binary_size(version: &str, size: usize) -> io::Result<ServerPayload> {
    let mut binary = vec![0; size];
    binary[..4].copy_from_slice(&0xfeedfacfu32.to_le_bytes());
    binary[4..8].copy_from_slice(&0x0100000cu32.to_le_bytes());
    binary[12..16].copy_from_slice(&2u32.to_le_bytes());
    ServerPayload::capture(ServerTarget::MacOsArm64, binary, |path| {
        Ok(if path == "Cargo.toml" {
            format!("[workspace.package]\nversion='{version}'\n").into_bytes()
        } else {
            format!("captured {path}\n").into_bytes()
        })
    })
    .map_err(io::Error::other)
}

#[test]
fn unix_server_archives_preserve_owned_bytes_and_reproducible_regular_file_metadata(
) -> io::Result<()> {
    for version in ["0.1.0".to_owned(), format!("0.1.0-{}", "a".repeat(58))] {
        let payload = payload(&version)?;
        let mut first = Vec::new();
        write_server_tar_gzip(&payload, &mut first)?;
        let mut second = Vec::new();
        write_server_tar_gzip(&payload, &mut second)?;
        assert_eq!(first, second);
        let root = ArchiveRoot::new(payload.name().root()).map_err(io::Error::other)?;
        let archive = read_regular_gzip(Cursor::new(&first), &root, ArchiveLimits::SERVER)
            .map_err(io::Error::other)?;
        assert_eq!(archive.files().len(), 9);
        for ((expected_path, expected), actual) in payload.files().zip(archive.files()) {
            assert_eq!(actual.path().as_str(), expected_path);
            assert_eq!(actual.bytes(), expected);
        }
        let mut gzip = flate2::read::GzDecoder::new(Cursor::new(&first));
        let header = gzip
            .header()
            .ok_or_else(|| io::Error::other("gzip header missing"))?;
        assert_eq!(header.mtime(), 0);
        assert_eq!(header.operating_system(), 255);
        assert!(header.filename().is_none());
        let mut tar = tar::Archive::new(&mut gzip);
        let mut count = 0;
        for entry in tar.entries()?.raw(true) {
            let mut entry = entry?;
            count += 1;
            assert!(entry.header().entry_type().is_file());
            assert_eq!(entry.header().uid()?, 0);
            assert_eq!(entry.header().gid()?, 0);
            assert_eq!(entry.header().mtime()?, 0);
            let path = entry.path()?;
            let binary = path.ends_with("bin/rusterisk");
            assert_eq!(entry.header().mode()?, if binary { 0o755 } else { 0o644 });
            io::copy(&mut entry, &mut io::sink())?;
        }
        assert_eq!(count, 9);
    }
    Ok(())
}

struct Failure {
    remaining: usize,
    flush: bool,
}
impl Write for Failure {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.remaining {
            return Err(io::Error::other("fixture write failed"));
        }
        self.remaining -= bytes.len();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        if self.flush {
            Err(io::Error::other("fixture flush failed"))
        } else {
            Ok(())
        }
    }
}

#[test]
fn unix_archive_creation_reports_compression_and_final_flush_failures() -> io::Result<()> {
    let payload = payload("0.1.0")?;
    for writer in [
        Failure {
            remaining: 12,
            flush: false,
        },
        Failure {
            remaining: usize::MAX,
            flush: true,
        },
    ] {
        assert!(write_server_tar_gzip(&payload, writer).is_err());
    }
    Ok(())
}

#[test]
fn server_binary_limits_preserve_the_smaller_sdk_archive_profile() -> io::Result<()> {
    let payload = payload_with_binary_size("0.1.0", 8 * 1024 * 1024 + 1)?;
    let mut bytes = Vec::new();
    write_server_tar_gzip(&payload, &mut bytes)?;
    let root = ArchiveRoot::new(payload.name().root()).map_err(io::Error::other)?;
    assert!(read_regular_gzip(Cursor::new(&bytes), &root, ArchiveLimits::SDK).is_err());
    assert_eq!(
        read_regular_gzip(Cursor::new(&bytes), &root, ArchiveLimits::SERVER)
            .map_err(io::Error::other)?
            .files()
            .len(),
        9
    );
    Ok(())
}
