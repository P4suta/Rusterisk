use super::{ServerCandidate, ServerOciProfile};
use crate::regular_archive::ArchivePathError;
use std::{
    collections::BTreeMap,
    fmt,
    io::{self, BufRead},
};

mod graph;
mod model;
mod stream;

#[derive(Debug, Clone, Copy)]
pub enum OciArchiveLimit {
    Archive,
    Blob,
    Json,
    Entries,
    Expanded,
    Graph,
}

pub enum OciArchiveError {
    Io(io::Error),
    Json(serde_json::Error),
    Path(ArchivePathError),
    Limit(OciArchiveLimit),
    ExpectedFiles,
    Layout,
    Entry,
    Duplicate,
    Trailing,
    Descriptor,
    Index,
    Platform,
    Configuration,
    Application,
    Attestation,
}

impl fmt::Display for OciArchiveError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::Limit(limit) => {
                return write!(formatter, "OCI candidate exceeds the {limit:?} limit")
            }
            Self::Io(_) => "OCI archive input failed",
            Self::Json(_) => "OCI metadata has invalid JSON",
            Self::Path(_) => "OCI archive has an unsafe path",
            Self::ExpectedFiles => "OCI verification requires the nine bounded candidate files",
            Self::Layout => "OCI candidate layout is incomplete or unsupported",
            Self::Entry => "OCI candidate has an unsupported archive entry",
            Self::Duplicate => "OCI candidate has duplicate archive entries",
            Self::Trailing => "OCI archive has invalid termination or trailing content",
            Self::Descriptor => "OCI descriptor digest, size, or local content differs",
            Self::Index => "OCI candidate index does not select one runnable image",
            Self::Platform => "OCI candidate platform differs",
            Self::Configuration => "OCI execution settings or source labels differ",
            Self::Application => "OCI application files or metadata differ",
            Self::Attestation => "OCI attestation references are inconsistent",
        };
        formatter.write_str(message)
    }
}
impl fmt::Debug for OciArchiveError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}
impl std::error::Error for OciArchiveError {}
impl From<io::Error> for OciArchiveError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}
impl From<serde_json::Error> for OciArchiveError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}
impl From<ArchivePathError> for OciArchiveError {
    fn from(value: ArchivePathError) -> Self {
        Self::Path(value)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct OciArchiveLimits {
    archive: u64,
    blob: u64,
    json: u64,
    entries: usize,
    expanded: u64,
}
impl OciArchiveLimits {
    pub const SERVER: Self = Self {
        archive: 512 * 1024 * 1024,
        blob: 272 * 1024 * 1024,
        json: 1024 * 1024,
        entries: 128,
        expanded: 512 * 1024 * 1024,
    };
    pub fn constrain(
        self,
        archive: u64,
        blob: u64,
        json: u64,
        entries: usize,
        expanded: u64,
    ) -> io::Result<Self> {
        if archive == 0
            || archive > self.archive
            || blob == 0
            || blob > self.blob
            || json == 0
            || json > self.json
            || entries == 0
            || entries > self.entries
            || expanded == 0
            || expanded > self.expanded
        {
            return Err(io::Error::other(
                "OCI limits may only be reduced to positive values",
            ));
        }
        Ok(Self {
            archive,
            blob,
            json,
            entries,
            expanded,
        })
    }
}

#[derive(Debug)]
pub struct ServerOciContents {
    manifest: String,
    config: String,
    application: String,
    attestations: usize,
}
impl ServerOciContents {
    pub fn manifest_digest(&self) -> &str {
        &self.manifest
    }
    pub fn config_digest(&self) -> &str {
        &self.config
    }
    pub fn application_layer_digest(&self) -> &str {
        &self.application
    }
    pub const fn attestation_count(&self) -> usize {
        self.attestations
    }
}

type Expected<'a> = BTreeMap<&'a str, &'a [u8]>;
fn expected<'a>(
    profile: &ServerOciProfile,
    files: impl Iterator<Item = (&'a str, &'a [u8])>,
) -> Result<Expected<'a>, OciArchiveError> {
    let required = profile.target().required_files();
    let mut expected = BTreeMap::new();
    for (index, (path, bytes)) in files.enumerate() {
        let limit = if path == profile.target().binary_path() {
            256 * 1024 * 1024
        } else {
            65_536
        };
        if index >= 9
            || !required.contains(&path)
            || bytes.is_empty()
            || bytes.len() > limit
            || expected.insert(path, bytes).is_some()
        {
            return Err(OciArchiveError::ExpectedFiles);
        }
    }
    if expected.len() != required.len() {
        return Err(OciArchiveError::ExpectedFiles);
    }
    Ok(expected)
}

/// Checks candidate content without authenticating provenance or the base image identity.
pub fn check_server_oci_archive<'a>(
    profile: &ServerOciProfile,
    files: impl Iterator<Item = (&'a str, &'a [u8])>,
    input: impl BufRead,
    limits: OciArchiveLimits,
) -> Result<ServerOciContents, OciArchiveError> {
    let expected = expected(profile, files)?;
    let store = stream::outer(input, limits)?;
    let selected = graph::select(&store, profile, limits)?;
    let mut expanded = limits.expanded;
    let count = selected.manifest.layers.len();
    for (index, (layer, diff_id)) in selected
        .manifest
        .layers
        .iter()
        .zip(&selected.config.rootfs.diff_ids)
        .enumerate()
    {
        let bytes = graph::descriptor(&store, layer)?;
        let application = index + 1 == count;
        let (actual, size) = stream::layer(
            bytes,
            if application {
                Some((&expected, profile))
            } else {
                None
            },
            expanded,
        )?;
        expanded = expanded
            .checked_sub(size)
            .ok_or(OciArchiveError::Limit(OciArchiveLimit::Expanded))?;
        if &actual != diff_id {
            return Err(OciArchiveError::Descriptor);
        }
    }
    Ok(ServerOciContents {
        manifest: selected.descriptor.digest,
        config: selected.manifest.config.digest,
        application: selected
            .manifest
            .layers
            .last()
            .ok_or(OciArchiveError::Application)?
            .digest
            .clone(),
        attestations: selected.attestations,
    })
}

impl ServerCandidate {
    pub fn check_oci(&self, archive: &std::path::Path) -> io::Result<ServerOciContents> {
        let limits = OciArchiveLimits::SERVER;
        let metadata = std::fs::symlink_metadata(archive)?;
        if !metadata.is_file() || metadata.len() == 0 || metadata.len() > limits.archive {
            return Err(io::Error::other(
                "OCI candidate requires a bounded regular archive",
            ));
        }
        let profile = ServerOciProfile::new(
            self.name().target(),
            self.name().version(),
            self.source_digest(),
        )?;
        check_server_oci_archive(
            &profile,
            self.files(),
            io::BufReader::new(std::fs::File::open(archive)?),
            limits,
        )
        .map_err(io::Error::other)
    }
}
