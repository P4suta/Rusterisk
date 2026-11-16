use std::io::{self, Cursor, Read, Write};
use xtask::server_distribution::{
    write_server_tar_gzip, write_server_zip, ServerPayload, ServerTarget,
};

fn payload() -> io::Result<ServerPayload> {
    let mut binary = vec![0; 256];
    binary[..2].copy_from_slice(b"MZ");
    binary[60..64].copy_from_slice(&64u32.to_le_bytes());
    binary[64..68].copy_from_slice(b"PE\0\0");
    binary[68..70].copy_from_slice(&0x8664u16.to_le_bytes());
    binary[70..72].copy_from_slice(&1u16.to_le_bytes());
    binary[84..86].copy_from_slice(&112u16.to_le_bytes());
    binary[86..88].copy_from_slice(&2u16.to_le_bytes());
    binary[88..90].copy_from_slice(&0x20bu16.to_le_bytes());
    binary[104..108].copy_from_slice(&4096u32.to_le_bytes());
    binary[156..158].copy_from_slice(&3u16.to_le_bytes());
    ServerPayload::capture(ServerTarget::WindowsX86_64, binary, |path| {
        Ok(if path == "Cargo.toml" {
            b"[workspace.package]\nversion='0.1.0'\n".to_vec()
        } else {
            format!("captured {path}\n").into_bytes()
        })
    })
    .map_err(io::Error::other)
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
fn zip_creation_reports_entry_directory_and_final_flush_failures() -> io::Result<()> {
    let payload = payload()?;
    let mut complete = Vec::new();
    write_server_zip(&payload, &mut complete)?;
    let archive = zip::ZipArchive::new(Cursor::new(&complete)).map_err(io::Error::other)?;
    let directory = usize::try_from(archive.central_directory_start()).map_err(io::Error::other)?;
    for writer in [
        Failure {
            remaining: 12,
            flush: false,
        },
        Failure {
            remaining: directory + 2,
            flush: false,
        },
        Failure {
            remaining: usize::MAX,
            flush: true,
        },
    ] {
        assert!(write_server_zip(&payload, writer).is_err());
    }
    Ok(())
}

#[test]
fn server_archive_writers_reject_the_other_target_format_before_output() -> io::Result<()> {
    let windows = payload()?;
    let mut output = Vec::new();
    assert!(write_server_tar_gzip(&windows, &mut output).is_err());
    assert!(output.is_empty());
    let mut binary = vec![0; 32];
    binary[..4].copy_from_slice(&0xfeedfacfu32.to_le_bytes());
    binary[4..8].copy_from_slice(&0x0100000cu32.to_le_bytes());
    binary[12..16].copy_from_slice(&2u32.to_le_bytes());
    let unix = ServerPayload::capture(ServerTarget::MacOsArm64, binary, |path| {
        Ok(if path == "Cargo.toml" {
            b"[workspace.package]\nversion='0.1.0'\n".to_vec()
        } else {
            format!("captured {path}\n").into_bytes()
        })
    })
    .map_err(io::Error::other)?;
    assert!(write_server_zip(&unix, &mut output).is_err());
    assert!(output.is_empty());
    Ok(())
}

#[test]
fn windows_server_archives_use_deflate_and_preserve_owned_files_with_fixed_metadata(
) -> io::Result<()> {
    let payload = payload()?;
    let mut bytes = Vec::new();
    write_server_zip(&payload, &mut bytes)?;
    let mut again = Vec::new();
    write_server_zip(&payload, &mut again)?;
    assert_eq!(bytes, again);
    let mut archive = zip::ZipArchive::new(Cursor::new(&bytes)).map_err(io::Error::other)?;
    assert_eq!(archive.len(), 9);
    let time = zip::DateTime::from_date_and_time(1980, 1, 1, 0, 0, 0).map_err(io::Error::other)?;
    for (index, (path, expected)) in payload.files().enumerate() {
        let mut file = archive.by_index(index).map_err(io::Error::other)?;
        assert_eq!(
            file.name_raw(),
            format!("{}/{path}", payload.name().root()).as_bytes()
        );
        assert_eq!(file.compression(), zip::CompressionMethod::Deflated);
        assert!(file.is_file());
        assert!(!file.is_symlink());
        assert!(!file.encrypted());
        assert_eq!(file.last_modified(), Some(time));
        assert_eq!(
            file.unix_mode().map(|mode| mode & 0o7777),
            Some(if path == "bin/rusterisk.exe" {
                0o755
            } else {
                0o644
            })
        );
        let mut found = Vec::new();
        file.read_to_end(&mut found)?;
        assert_eq!(found, expected);
    }
    Ok(())
}
