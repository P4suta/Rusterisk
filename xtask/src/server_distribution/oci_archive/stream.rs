use super::model::{self, Layout};
use super::{Expected, OciArchiveError, OciArchiveLimit, OciArchiveLimits};
use crate::{
    regular_archive::{ArchivePath, ArchiveRoot},
    server_distribution::ServerOciProfile,
};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{self, BufRead, Cursor, Read, Take},
};

pub(super) struct Store {
    pub blobs: BTreeMap<String, Vec<u8>>,
    pub index: Vec<u8>,
}
pub(super) fn digest(bytes: &[u8]) -> String {
    finish(Sha256::new_with_prefix(bytes))
}
fn finish(hash: Sha256) -> String {
    let hex: String = hash
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("sha256:{hex}")
}

fn tail(reader: &mut impl Read) -> Result<u64, OciArchiveError> {
    let mut count = 0;
    let mut buffer = [0; 8192];
    loop {
        let size = reader.read(&mut buffer)?;
        if size == 0 {
            break;
        }
        if buffer[..size].iter().any(|byte| *byte != 0) {
            return Err(OciArchiveError::Trailing);
        }
        count += size as u64;
    }
    Ok(count)
}

fn path(raw: &[u8], root: &ArchiveRoot, directory: bool) -> Result<String, OciArchiveError> {
    let raw = if directory {
        raw.strip_suffix(b"/").unwrap_or(raw)
    } else {
        raw
    };
    let value = std::str::from_utf8(raw).map_err(|_| OciArchiveError::Entry)?;
    Ok(
        ArchivePath::parse(root, format!("{}/{value}", root.as_str()).as_bytes(), 4)?
            .as_str()
            .to_owned(),
    )
}

pub(super) fn outer(
    input: impl BufRead,
    limits: OciArchiveLimits,
) -> Result<Store, OciArchiveError> {
    let mut archive = tar::Archive::new(input.take(limits.archive + 1));
    let mut store = Store {
        blobs: BTreeMap::new(),
        index: Vec::new(),
    };
    let root = ArchiveRoot::new("oci")?;
    let mut names = BTreeSet::new();
    let mut layout = None;
    let operation = (|| {
        for entry in archive.entries()?.raw(true) {
            let mut entry = entry?;
            if names.len() >= limits.entries {
                return Err(OciArchiveError::Limit(OciArchiveLimit::Entries));
            }
            let directory = entry.header().entry_type().is_dir();
            if !directory && !entry.header().entry_type().is_file() {
                return Err(OciArchiveError::Entry);
            }
            let name = path(&entry.path_bytes(), &root, directory)?;
            if !names.insert(name.to_ascii_lowercase()) {
                return Err(OciArchiveError::Duplicate);
            }
            let size = entry.header().size()?;
            if directory {
                if !matches!(name.as_str(), "blobs" | "blobs/sha256") || size != 0 {
                    return Err(OciArchiveError::Entry);
                }
                continue;
            }
            if size > limits.blob {
                return Err(OciArchiveError::Limit(OciArchiveLimit::Blob));
            }
            if matches!(name.as_str(), "index.json" | "oci-layout") && size > limits.json {
                return Err(OciArchiveError::Limit(OciArchiveLimit::Json));
            }
            let mut bytes = Vec::new();
            entry.by_ref().take(size + 1).read_to_end(&mut bytes)?;
            if bytes.len() as u64 != size {
                return Err(OciArchiveError::Trailing);
            }
            match name.as_str() {
                "oci-layout" => layout = Some(model::parse::<Layout>(&bytes, limits)?),
                "index.json" => store.index = bytes,
                _ => {
                    let hex = name
                        .strip_prefix("blobs/sha256/")
                        .ok_or(OciArchiveError::Layout)?;
                    let expected = format!("sha256:{hex}");
                    if model::sha256(&expected).is_none() || digest(&bytes) != expected {
                        return Err(OciArchiveError::Descriptor);
                    }
                    store.blobs.insert(expected, bytes);
                }
            }
        }
        Ok(())
    })();
    let mut input = archive.into_inner();
    operation.map_err(|error| {
        if input.limit() == 0 {
            OciArchiveError::Limit(OciArchiveLimit::Archive)
        } else {
            error
        }
    })?;
    let padding = tail(&mut input)?;
    let size = limits.archive + 1 - input.limit();
    if input.limit() == 0 {
        return Err(OciArchiveError::Limit(OciArchiveLimit::Archive));
    }
    if padding < 512 || !size.is_multiple_of(512) {
        return Err(OciArchiveError::Trailing);
    }
    if layout.is_none_or(|layout| layout.version != "1.0.0")
        || store.index.is_empty()
        || store.blobs.is_empty()
    {
        return Err(OciArchiveError::Layout);
    }
    Ok(store)
}

struct Hashed<R> {
    input: Take<R>,
    hash: Sha256,
}
impl<R: Read> Read for Hashed<R> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let count = self.input.read(bytes)?;
        self.hash.update(&bytes[..count]);
        Ok(count)
    }
}

fn application<R: Read>(
    archive: &mut tar::Archive<R>,
    expected: &Expected<'_>,
    profile: &ServerOciProfile,
) -> Result<(), OciArchiveError> {
    let root = ArchiveRoot::new("oci")?;
    let mut names = BTreeSet::new();
    let mut files = 0;
    for entry in archive.entries()?.raw(true) {
        let mut entry = entry?;
        if names.len() >= 32 {
            return Err(OciArchiveError::Limit(OciArchiveLimit::Entries));
        }
        let header = entry.header().clone();
        let directory = header.entry_type().is_dir();
        if !directory && !header.entry_type().is_file() {
            return Err(OciArchiveError::Entry);
        }
        let name = path(&entry.path_bytes(), &root, directory)?;
        if !names.insert(name.to_ascii_lowercase()) {
            return Err(OciArchiveError::Duplicate);
        }
        if directory {
            if !matches!(
                name.as_str(),
                "opt"
                    | "opt/rusterisk"
                    | "opt/rusterisk/bin"
                    | "opt/rusterisk/config"
                    | "opt/rusterisk/data"
            ) || header.size()? != 0
                || header.mode()? != 0o755
            {
                return Err(OciArchiveError::Application);
            }
            if name == "opt/rusterisk/data" && (header.uid()? != 65532 || header.gid()? != 65532) {
                return Err(OciArchiveError::Application);
            }
            continue;
        }
        let relative = name
            .strip_prefix("opt/rusterisk/")
            .ok_or(OciArchiveError::Application)?;
        let expected = expected.get(relative).ok_or(OciArchiveError::Application)?;
        let mode = if relative == profile.target().binary_path() {
            0o755
        } else {
            0o644
        };
        if header.size()? != expected.len() as u64
            || header.mode()? != mode
            || header.uid()? != 65532
            || header.gid()? != 65532
            || header.mtime()? != 0
        {
            return Err(OciArchiveError::Application);
        }
        let mut offset = 0;
        let mut buffer = [0; 8192];
        loop {
            let count = entry.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            let end = offset + count;
            if expected.get(offset..end) != Some(&buffer[..count]) {
                return Err(OciArchiveError::Application);
            }
            offset = end;
        }
        if offset != expected.len() {
            return Err(OciArchiveError::Application);
        }
        files += 1;
    }
    if files != 9 || !names.contains("opt/rusterisk/data") {
        return Err(OciArchiveError::Application);
    }
    Ok(())
}

pub(super) fn layer(
    bytes: &[u8],
    expected: Option<(&Expected<'_>, &ServerOciProfile)>,
    remaining: u64,
) -> Result<(String, u64), OciArchiveError> {
    let limit = if expected.is_some() {
        remaining.min(272 * 1024 * 1024)
    } else {
        remaining
    };
    let decoder = flate2::bufread::GzDecoder::new(Cursor::new(bytes));
    let hashed = Hashed {
        input: decoder.take(limit + 1),
        hash: Sha256::new(),
    };
    let reader = if let Some((expected, profile)) = expected {
        let mut archive = tar::Archive::new(hashed);
        let result = application(&mut archive, expected, profile);
        let mut reader = archive.into_inner();
        result.map_err(|error| {
            if reader.input.limit() == 0 {
                OciArchiveError::Limit(OciArchiveLimit::Expanded)
            } else {
                error
            }
        })?;
        let padding = tail(&mut reader)?;
        let size = limit + 1 - reader.input.limit();
        if reader.input.limit() == 0 {
            return Err(OciArchiveError::Limit(OciArchiveLimit::Expanded));
        }
        if padding < 512 || !size.is_multiple_of(512) {
            return Err(OciArchiveError::Trailing);
        }
        reader
    } else {
        let mut reader = hashed;
        let mut buffer = [0; 8192];
        while reader.read(&mut buffer)? != 0 {}
        reader
    };
    if reader.input.limit() == 0 {
        return Err(OciArchiveError::Limit(OciArchiveLimit::Expanded));
    }
    let size = limit + 1 - reader.input.limit();
    let decoder = reader.input.into_inner();
    if decoder.into_inner().position() != bytes.len() as u64 {
        return Err(OciArchiveError::Trailing);
    }
    Ok((finish(reader.hash), size))
}
