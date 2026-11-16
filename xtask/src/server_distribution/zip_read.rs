use super::{ArchiveFormat, ServerCandidateName, ServerPayload};
use std::io::{self, Cursor, Read};
use zip::{
    read::{ArchiveOffset, Config},
    CompressionMethod, DateTime, ZipArchive,
};

fn invalid() -> io::Error {
    io::Error::other("server zip differs from the bounded candidate profile")
}

fn range(bytes: &[u8], offset: usize, size: usize) -> io::Result<&[u8]> {
    bytes
        .get(offset..offset.checked_add(size).ok_or_else(invalid)?)
        .ok_or_else(invalid)
}

fn short(bytes: &[u8], offset: usize) -> io::Result<u16> {
    Ok(u16::from_le_bytes(
        range(bytes, offset, 2)?.try_into().map_err(|_| invalid())?,
    ))
}

fn word(bytes: &[u8], offset: usize) -> io::Result<u32> {
    Ok(u32::from_le_bytes(
        range(bytes, offset, 4)?.try_into().map_err(|_| invalid())?,
    ))
}

fn offset(value: u32) -> io::Result<usize> {
    usize::try_from(value).map_err(|_| invalid())
}

fn directory(bytes: &[u8]) -> io::Result<usize> {
    if bytes.len() > 272 * 1024 * 1024 || !bytes.starts_with(b"PK\x03\x04") {
        return Err(invalid());
    }
    let end = bytes.len().checked_sub(22).ok_or_else(invalid)?;
    let record = range(bytes, end, 22)?;
    if !record.starts_with(b"PK\x05\x06")
        || short(record, 4)? != 0
        || short(record, 6)? != 0
        || short(record, 8)? != 9
        || short(record, 10)? != 9
        || short(record, 20)? != 0
    {
        return Err(invalid());
    }
    let size = offset(word(record, 12)?)?;
    let start = offset(word(record, 16)?)?;
    if size > 16_384 || start.checked_add(size) != Some(end) {
        return Err(invalid());
    }
    let central = range(bytes, start, size)?;
    let mut position = 0;
    for _ in 0..9 {
        let header = range(central, position, 46)?;
        if !header.starts_with(b"PK\x01\x02")
            || short(header, 8)? != 8
            || short(header, 10)? != 8
            || short(header, 30)? != 0
            || short(header, 32)? != 0
            || short(header, 34)? != 0
            || word(header, 20)? as u64 > 272 * 1024 * 1024
            || word(header, 24)? as u64 > 256 * 1024 * 1024
        {
            return Err(invalid());
        }
        let name = usize::from(short(header, 28)?);
        if !(1..=240).contains(&name) {
            return Err(invalid());
        }
        position = position.checked_add(46 + name).ok_or_else(invalid)?;
    }
    if position != central.len() {
        return Err(invalid());
    }
    Ok(start)
}

fn contents(reader: &mut impl Read, expected: &[u8]) -> io::Result<()> {
    let mut position = 0usize;
    let mut buffer = [0; 8192];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            return if position == expected.len() {
                Ok(())
            } else {
                Err(invalid())
            };
        }
        let end = position.checked_add(count).ok_or_else(invalid)?;
        if expected.get(position..end) != Some(&buffer[..count]) {
            return Err(invalid());
        }
        position = end;
    }
}

pub(super) fn binary(name: &ServerCandidateName, bytes: &[u8]) -> io::Result<Vec<u8>> {
    directory(bytes)?;
    let mut archive = ZipArchive::with_config(
        Config {
            archive_offset: ArchiveOffset::Known(0),
        },
        Cursor::new(bytes),
    )?;
    let path = format!("{}/{}", name.root(), name.target().binary_path());
    let mut file = archive.by_name(&path)?;
    if !file.is_file()
        || file.is_symlink()
        || file.encrypted()
        || file.size() == 0
        || file.size() > 256 * 1024 * 1024
        || file.compression() != CompressionMethod::Deflated
    {
        return Err(invalid());
    }
    let expected = file.size();
    let mut bytes = Vec::new();
    file.by_ref().take(expected + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 != expected {
        return Err(invalid());
    }
    Ok(bytes)
}

pub fn check_server_zip(payload: &ServerPayload, bytes: &[u8]) -> io::Result<()> {
    if payload.name().target().format() != ArchiveFormat::Zip {
        return Err(invalid());
    }
    let directory = directory(bytes)?;
    let mut archive = ZipArchive::with_config(
        Config {
            archive_offset: ArchiveOffset::Known(0),
        },
        Cursor::new(bytes),
    )?;
    if archive.len() != 9
        || archive.has_duplicate_names()
        || archive.central_directory_start() != directory as u64
        || archive.offset() != 0
        || !archive.comment().is_empty()
        || archive.raw_zip64_extensible_data_sector().is_some()
    {
        return Err(invalid());
    }
    archive.validate_header_consistency()?;
    let time = DateTime::from_date_and_time(1980, 1, 1, 0, 0, 0).map_err(io::Error::other)?;
    let mut position = 0usize;
    for (index, (path, expected)) in payload.files().enumerate() {
        let mut file = archive.by_index(index)?;
        let name = format!("{}/{path}", payload.name().root());
        let mode = if path == payload.name().target().binary_path() {
            0o100755
        } else {
            0o100644
        };
        if file.name_raw() != name.as_bytes()
            || file.encrypted()
            || !file.is_file()
            || file.is_symlink()
            || file.compression() != CompressionMethod::Deflated
            || file.last_modified() != Some(time)
            || file.unix_mode() != Some(mode)
            || file.size() != expected.len() as u64
            || !file.comment_raw().is_empty()
            || file.header_start() != position as u64
        {
            return Err(invalid());
        }
        let local = range(bytes, position, 30)?;
        if !local.starts_with(b"PK\x03\x04")
            || short(local, 6)? != 8
            || short(local, 8)? != 8
            || short(local, 26)? as usize != name.len()
            || short(local, 28)? != 0
        {
            return Err(invalid());
        }
        let data = position.checked_add(30 + name.len()).ok_or_else(invalid)?;
        if file.data_start() != Some(data as u64) {
            return Err(invalid());
        }
        let descriptor = data
            .checked_add(usize::try_from(file.compressed_size()).map_err(|_| invalid())?)
            .ok_or_else(invalid)?;
        let record = range(bytes, descriptor, 16)?;
        if !record.starts_with(b"PK\x07\x08")
            || word(record, 4)? != file.crc32()
            || u64::from(word(record, 8)?) != file.compressed_size()
            || u64::from(word(record, 12)?) != file.size()
        {
            return Err(invalid());
        }
        position = descriptor.checked_add(16).ok_or_else(invalid)?;
        contents(&mut file, expected)?;
    }
    if position != directory {
        return Err(invalid());
    }
    Ok(())
}
