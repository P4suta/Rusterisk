use std::io::{self, Cursor, Read, Write};
use xtask::server_distribution::{
    check_server_zip, write_server_tar_gzip, write_server_zip, ServerPayload, ServerTarget,
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

#[test]
fn zip_candidates_must_match_every_captured_file_and_its_metadata() -> io::Result<()> {
    let payload = payload()?;
    let mut bytes = Vec::new();
    write_server_zip(&payload, &mut bytes)?;
    check_server_zip(&payload, &bytes)?;
    let end = bytes.len() - 22;
    let directory = usize::try_from(u32::from_le_bytes(
        bytes[end + 16..end + 20]
            .try_into()
            .map_err(io::Error::other)?,
    ))
    .map_err(io::Error::other)?;
    let mut parsed = zip::ZipArchive::new(Cursor::new(&bytes)).map_err(io::Error::other)?;
    let compressed = parsed.by_index(0)?.compressed_size();
    let name_size = usize::from(u16::from_le_bytes(
        bytes[26..28].try_into().map_err(io::Error::other)?,
    ));
    let data = 30 + name_size;
    let descriptor = data + usize::try_from(compressed).map_err(io::Error::other)?;
    for offset in [
        0,
        30,
        data,
        descriptor,
        descriptor + 4,
        descriptor + 8,
        descriptor + 12,
        directory,
        directory + 8,
        directory + 10,
        directory + 12,
        directory + 14,
        directory + 16,
        directory + 20,
        directory + 24,
        directory + 30,
        directory + 32,
        directory + 34,
        directory + 40,
        directory + 42,
        directory + 46,
        end + 4,
        end + 6,
        end + 8,
        end + 10,
        end + 12,
        end + 16,
        end + 20,
    ] {
        let mut corrupt = bytes.clone();
        corrupt[offset] ^= 1;
        assert!(
            check_server_zip(&payload, &corrupt).is_err(),
            "offset {offset}"
        );
    }
    for size in [0, 1, 21, 22, directory, bytes.len() - 1] {
        assert!(check_server_zip(&payload, &bytes[..size]).is_err());
    }
    let mut trailing = bytes.clone();
    trailing.push(0);
    assert!(check_server_zip(&payload, &trailing).is_err());
    let mut prefixed = b"prefix".to_vec();
    prefixed.extend_from_slice(&bytes);
    assert!(check_server_zip(&payload, &prefixed).is_err());
    Ok(())
}

#[test]
fn zip_readback_rejects_changed_regular_content_and_extra_entries() -> io::Result<()> {
    let payload = payload()?;
    for extra in [false, true] {
        let mut archive = zip::ZipWriter::new_stream(Vec::new());
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .last_modified_time(
                zip::DateTime::from_date_and_time(1980, 1, 1, 0, 0, 0).map_err(io::Error::other)?,
            );
        for (index, (path, bytes)) in payload.files().enumerate() {
            archive.start_file(
                format!("{}/{path}", payload.name().root()),
                options.unix_permissions(if index == 0 { 0o755 } else { 0o644 }),
            )?;
            if !extra && index == 1 {
                let mut changed = bytes.to_vec();
                changed[0] ^= 1;
                archive.write_all(&changed)?;
            } else {
                archive.write_all(bytes)?;
            }
        }
        if extra {
            archive.start_file(format!("{}/extra", payload.name().root()), options)?;
            archive.write_all(b"extra")?;
        }
        assert!(check_server_zip(&payload, &archive.finish()?.into_inner()).is_err());
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
