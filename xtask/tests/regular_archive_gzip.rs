use std::io::{BufReader, Cursor, Write};
use xtask::regular_archive::{
    read_regular_gzip, ArchiveLimit, ArchiveLimits, ArchiveRoot, RegularArchive,
    RegularArchiveError,
};

fn tar(entries: &[(&str, u8, &[u8])]) -> Result<Vec<u8>, std::io::Error> {
    let mut archive = tar::Builder::new(Vec::new());
    for (path, kind, bytes) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_entry_type(tar::EntryType::new(*kind));
        let field = header
            .as_mut_bytes()
            .get_mut(..path.len())
            .ok_or_else(|| std::io::Error::other("fixture path exceeds header"))?;
        field.copy_from_slice(path.as_bytes());
        header.set_cksum();
        archive.append(&header, *bytes)?;
    }
    archive.into_inner()
}

fn compress(bytes: &[u8]) -> Result<Vec<u8>, std::io::Error> {
    let mut encoded = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    encoded.write_all(bytes)?;
    encoded.finish()
}

fn decode(bytes: &[u8], limits: ArchiveLimits) -> Result<RegularArchive, RegularArchiveError> {
    let root = ArchiveRoot::new("sdk-0.1.0").map_err(RegularArchiveError::Path)?;
    read_regular_gzip(
        BufReader::with_capacity(1, Cursor::new(bytes)),
        &root,
        limits,
    )
}

#[test]
fn one_member_regular_archives_preserve_bytes_through_partial_reads() -> Result<(), std::io::Error>
{
    let raw = tar(&[
        ("sdk-0.1.0/Cargo.toml", b'0', b"[package]\n"),
        (
            "sdk-0.1.0/src/lib.rs",
            0,
            b"pub const READY: bool = true;\n",
        ),
    ])?;
    let encoded = compress(&raw)?;
    let result = decode(&encoded, ArchiveLimits::SDK);
    assert!(result.is_ok(), "{result:?}");
    if let Ok(archive) = result {
        assert_eq!(archive.files().len(), 2);
        assert_eq!(archive.files()[0].path().as_str(), "Cargo.toml");
        assert_eq!(archive.files()[0].bytes(), b"[package]\n");
        assert_eq!(archive.expanded_bytes(), raw.len() as u64);
    }
    Ok(())
}

#[test]
fn every_nonregular_tar_type_is_refused_before_materialization() -> Result<(), std::io::Error> {
    for kind in *b"1234567xgLKS" {
        let encoded = compress(&tar(&[("sdk-0.1.0/entry", kind, b"")])?)?;
        let result = decode(&encoded, ArchiveLimits::SDK);
        assert!(
            matches!(result, Err(RegularArchiveError::UnsupportedEntry)),
            "{kind}: {result:?}"
        );
    }
    Ok(())
}

#[test]
fn traversal_case_collisions_and_file_parent_conflicts_are_refused() -> Result<(), std::io::Error> {
    let traversal = compress(&tar(&[("sdk-0.1.0/../outside", b'0', b"x")])?)?;
    assert!(matches!(
        decode(&traversal, ArchiveLimits::SDK),
        Err(RegularArchiveError::Path(_))
    ));
    let duplicate = compress(&tar(&[
        ("sdk-0.1.0/File.rs", b'0', b"x"),
        ("sdk-0.1.0/file.rs", b'0', b"y"),
    ])?)?;
    assert!(matches!(
        decode(&duplicate, ArchiveLimits::SDK),
        Err(RegularArchiveError::Duplicate)
    ));
    for entries in [
        [
            ("sdk-0.1.0/src", b'0', b"".as_slice()),
            ("sdk-0.1.0/SRC/lib.rs", b'0', b"".as_slice()),
        ],
        [
            ("sdk-0.1.0/src/lib.rs", b'0', b"".as_slice()),
            ("sdk-0.1.0/SRC", b'0', b"".as_slice()),
        ],
    ] {
        let encoded = compress(&tar(&entries)?)?;
        assert!(matches!(
            decode(&encoded, ArchiveLimits::SDK),
            Err(RegularArchiveError::Hierarchy)
        ));
    }
    Ok(())
}

#[test]
fn gzip_footer_concatenation_and_trailing_bytes_are_not_ignored() -> Result<(), std::io::Error> {
    let valid = compress(&tar(&[("sdk-0.1.0/file", b'0', b"data")])?)?;
    let mut bad_crc = valid.clone();
    let index = bad_crc
        .len()
        .checked_sub(8)
        .ok_or_else(|| std::io::Error::other("gzip footer missing"))?;
    let byte = bad_crc
        .get_mut(index)
        .ok_or_else(|| std::io::Error::other("gzip CRC missing"))?;
    *byte ^= 1;
    assert!(matches!(
        decode(&bad_crc, ArchiveLimits::SDK),
        Err(RegularArchiveError::Io(_))
    ));
    let mut truncated = valid.clone();
    truncated.pop();
    assert!(decode(&truncated, ArchiveLimits::SDK).is_err());
    for suffix in [valid.as_slice(), b"trailing".as_slice(), b"\0".as_slice()] {
        let mut concatenated = valid.clone();
        concatenated.extend_from_slice(suffix);
        assert!(matches!(
            decode(&concatenated, ArchiveLimits::SDK),
            Err(RegularArchiveError::Trailing)
        ));
    }
    Ok(())
}

#[test]
fn tar_checksum_padding_and_two_block_termination_are_checked() -> Result<(), std::io::Error> {
    let valid = tar(&[("sdk-0.1.0/file", b'0', b"data")])?;
    let mut bad_checksum = valid.clone();
    let byte = bad_checksum
        .first_mut()
        .ok_or_else(|| std::io::Error::other("tar header missing"))?;
    *byte ^= 1;
    assert!(decode(&compress(&bad_checksum)?, ArchiveLimits::SDK).is_err());
    let mut one_zero = valid.clone();
    one_zero.truncate(one_zero.len().saturating_sub(512));
    assert!(matches!(
        decode(&compress(&one_zero)?, ArchiveLimits::SDK),
        Err(RegularArchiveError::Termination)
    ));
    for byte in [0, 1] {
        let mut trailing = valid.clone();
        trailing.push(byte);
        assert!(matches!(
            decode(&compress(&trailing)?, ArchiveLimits::SDK),
            Err(RegularArchiveError::Termination)
        ));
    }
    Ok(())
}

#[test]
fn limits_accept_exact_boundaries_and_refuse_each_exceeded_resource() -> Result<(), std::io::Error>
{
    let raw = tar(&[("sdk-0.1.0/file", b'0', b"data")])?;
    let encoded = compress(&raw)?;
    let exact = ArchiveLimits::SDK.constrain(encoded.len() as u64, raw.len() as u64, 4, 1, 1);
    assert!(exact.is_ok(), "{exact:?}");
    if let Ok(exact) = exact {
        assert!(decode(&encoded, exact).is_ok());
    }
    for (compressed, expanded, file, expected) in [
        (
            encoded.len() as u64 - 1,
            raw.len() as u64,
            4,
            ArchiveLimit::Compressed,
        ),
        (
            encoded.len() as u64,
            raw.len() as u64 - 1,
            4,
            ArchiveLimit::Expanded,
        ),
        (
            encoded.len() as u64,
            raw.len() as u64,
            3,
            ArchiveLimit::File,
        ),
    ] {
        let limits = ArchiveLimits::SDK.constrain(compressed, expanded, file, 1, 1);
        assert!(limits.is_ok(), "{limits:?}");
        if let Ok(limits) = limits {
            assert!(
                matches!(decode(&encoded, limits), Err(RegularArchiveError::Limit(found)) if found == expected)
            );
        }
    }
    let two = compress(&tar(&[
        ("sdk-0.1.0/one", b'0', b""),
        ("sdk-0.1.0/two", b'0', b""),
    ])?)?;
    let limits = ArchiveLimits::SDK.constrain(4096, 4096, 1024, 1, 2);
    assert!(limits.is_ok(), "{limits:?}");
    if let Ok(limits) = limits {
        assert!(matches!(
            decode(&two, limits),
            Err(RegularArchiveError::Limit(ArchiveLimit::Entries))
        ));
    }
    assert!(matches!(
        ArchiveLimits::SDK.constrain(0, 4096, 1024, 1, 1),
        Err(RegularArchiveError::InvalidLimits)
    ));
    assert!(matches!(
        ArchiveLimits::SDK.constrain(17 * 1024 * 1024, 4096, 1024, 1, 1),
        Err(RegularArchiveError::InvalidLimits)
    ));
    Ok(())
}

struct Unreadable;

impl std::io::Read for Unreadable {
    fn read(&mut self, _buffer: &mut [u8]) -> Result<usize, std::io::Error> {
        Err(std::io::ErrorKind::PermissionDenied.into())
    }
}

impl std::io::BufRead for Unreadable {
    fn fill_buf(&mut self) -> Result<&[u8], std::io::Error> {
        Err(std::io::ErrorKind::PermissionDenied.into())
    }

    fn consume(&mut self, _amount: usize) {}
}

#[test]
fn input_failures_keep_their_io_cause() -> Result<(), xtask::regular_archive::ArchivePathError> {
    let root = ArchiveRoot::new("sdk-0.1.0")?;
    let result = read_regular_gzip(Unreadable, &root, ArchiveLimits::SDK);
    assert!(
        matches!(result, Err(RegularArchiveError::Io(error)) if error.kind() == std::io::ErrorKind::PermissionDenied)
    );
    Ok(())
}

#[test]
fn empty_archives_and_attempts_to_expand_limits_are_refused() -> Result<(), std::io::Error> {
    let empty = compress(&tar(&[])?)?;
    assert!(matches!(
        decode(&empty, ArchiveLimits::SDK),
        Err(RegularArchiveError::Empty)
    ));
    for (compressed, expanded, file, entries, depth) in [
        (4096, 0, 1024, 1, 1),
        (4096, 33 * 1024 * 1024, 1024, 1, 1),
        (4096, 4096, 0, 1, 1),
        (4096, 4096, 4097, 1, 1),
        (4096, 4096, 1024, 0, 1),
        (4096, 4096, 1024, 1025, 1),
        (4096, 4096, 1024, 1, 0),
        (4096, 4096, 1024, 1, 17),
    ] {
        assert!(matches!(
            ArchiveLimits::SDK.constrain(compressed, expanded, file, entries, depth),
            Err(RegularArchiveError::InvalidLimits)
        ));
    }
    if let Ok(narrow) = ArchiveLimits::SDK.constrain(4096, 4096, 1024, 1, 1) {
        assert!(matches!(
            narrow.constrain(4097, 4096, 1024, 1, 1),
            Err(RegularArchiveError::InvalidLimits)
        ));
    }
    Ok(())
}
