use super::{
    model::{self, Configuration, Descriptor, Index, Manifest, Statement},
    stream::Store,
    OciArchiveError, OciArchiveLimit, OciArchiveLimits,
};
use crate::server_distribution::ServerOciProfile;
use std::collections::BTreeSet;

pub(super) struct Selected {
    pub descriptor: Descriptor,
    pub manifest: Manifest,
    pub config: Configuration,
    pub attestations: usize,
}
pub(super) fn descriptor<'a>(
    store: &'a Store,
    descriptor: &Descriptor,
) -> Result<&'a [u8], OciArchiveError> {
    if !model::untagged(&descriptor.annotations) {
        return Err(OciArchiveError::Layout);
    }
    if model::sha256(&descriptor.digest).is_none()
        || descriptor.data.is_some()
        || descriptor.urls.is_some()
    {
        return Err(OciArchiveError::Descriptor);
    }
    let bytes = store
        .blobs
        .get(&descriptor.digest)
        .ok_or(OciArchiveError::Descriptor)?;
    if bytes.len() as u64 != descriptor.size {
        return Err(OciArchiveError::Descriptor);
    }
    Ok(bytes)
}
fn referenced<'a>(
    store: &'a Store,
    entry: &Descriptor,
    used: &mut BTreeSet<String>,
) -> Result<&'a [u8], OciArchiveError> {
    let bytes = descriptor(store, entry)?;
    used.insert(entry.digest.clone());
    Ok(bytes)
}
struct Walk {
    used: BTreeSet<String>,
    records: Vec<(Descriptor, Manifest)>,
    remaining: usize,
}
fn walk(
    store: &Store,
    index: Index,
    limits: OciArchiveLimits,
    depth: usize,
    state: &mut Walk,
) -> Result<(), OciArchiveError> {
    if !model::untagged(&index.annotations) {
        return Err(OciArchiveError::Layout);
    }
    state.remaining = state
        .remaining
        .checked_sub(1)
        .ok_or(OciArchiveError::Limit(OciArchiveLimit::Graph))?;
    if depth > 4 || index.manifests.len() > 32 || state.records.len() > 32 {
        return Err(OciArchiveError::Limit(OciArchiveLimit::Graph));
    }
    if index.schema_version != 2
        || index
            .media_type
            .as_deref()
            .is_some_and(|value| value != model::INDEX)
    {
        return Err(OciArchiveError::Index);
    }
    for entry in index.manifests {
        let bytes = referenced(store, &entry, &mut state.used)?;
        match entry.media_type.as_str() {
            model::INDEX => walk(
                store,
                model::parse(bytes, limits)?,
                limits,
                depth + 1,
                state,
            )?,
            model::MANIFEST => {
                let manifest: Manifest = model::parse(bytes, limits)?;
                if !model::untagged(&manifest.annotations) {
                    return Err(OciArchiveError::Layout);
                }
                if manifest.schema_version != 2
                    || manifest
                        .media_type
                        .as_deref()
                        .is_some_and(|value| value != model::MANIFEST)
                    || manifest.layers.len() > 64
                {
                    return Err(OciArchiveError::Index);
                }
                state.records.push((entry, manifest));
                if state.records.len() > 32 {
                    return Err(OciArchiveError::Limit(OciArchiveLimit::Graph));
                }
            }
            _ => return Err(OciArchiveError::Index),
        }
    }
    Ok(())
}

fn attestation(
    store: &Store,
    entry: &Descriptor,
    manifest: &Manifest,
    runnable: &Descriptor,
    limits: OciArchiveLimits,
    used: &mut BTreeSet<String>,
) -> Result<(), OciArchiveError> {
    let platform = entry
        .platform
        .as_ref()
        .ok_or(OciArchiveError::Attestation)?;
    if platform.os != "unknown"
        || platform.architecture != "unknown"
        || entry.annotations.get("vnd.docker.reference.digest") != Some(&runnable.digest)
        || manifest.layers.is_empty()
    {
        return Err(OciArchiveError::Attestation);
    }
    let config = referenced(store, &manifest.config, used)?;
    match manifest.artifact_type.as_deref() {
        Some("application/vnd.docker.attestation.manifest.v1+json") => {
            let subject = manifest
                .subject
                .as_ref()
                .ok_or(OciArchiveError::Attestation)?;
            if subject.digest != runnable.digest
                || subject.size != runnable.size
                || subject.media_type != model::MANIFEST
                || manifest.config.media_type != "application/vnd.oci.empty.v1+json"
                || config != b"{}"
            {
                return Err(OciArchiveError::Attestation);
            }
            referenced(store, subject, used)?;
        }
        None => {
            if manifest.config.media_type != model::CONFIG || manifest.subject.is_some() {
                return Err(OciArchiveError::Attestation);
            }
            let _: serde_json::Map<String, serde_json::Value> = model::parse(config, limits)?;
        }
        Some(_) => return Err(OciArchiveError::Attestation),
    }
    for layer in &manifest.layers {
        if layer.media_type != "application/vnd.in-toto+json" {
            return Err(OciArchiveError::Attestation);
        }
        let statement: Statement = model::parse(referenced(store, layer, used)?, limits)?;
        let hash = model::sha256(&runnable.digest).ok_or(OciArchiveError::Attestation)?;
        if !matches!(
            statement.kind.as_str(),
            "https://in-toto.io/Statement/v1" | "https://in-toto.io/Statement/v0.1"
        ) || statement.predicate_type.is_empty()
            || statement.subject.is_empty()
            || statement.subject.len() > 32
            || statement.subject.iter().any(|subject| {
                subject.digest.len() != 1
                    || subject.digest.get("sha256").map(String::as_str) != Some(hash)
            })
        {
            return Err(OciArchiveError::Attestation);
        }
    }
    Ok(())
}

pub(super) fn select(
    store: &Store,
    profile: &ServerOciProfile,
    limits: OciArchiveLimits,
) -> Result<Selected, OciArchiveError> {
    let index: Index = model::parse(&store.index, limits)?;
    let mut state = Walk {
        used: BTreeSet::new(),
        records: Vec::new(),
        remaining: 128,
    };
    walk(store, index, limits, 0, &mut state)?;
    let Walk {
        mut used, records, ..
    } = state;
    let mut runnable = None;
    let mut attestations = Vec::new();
    for (descriptor, manifest) in records {
        if descriptor
            .annotations
            .get("vnd.docker.reference.type")
            .map(String::as_str)
            == Some("attestation-manifest")
        {
            attestations.push((descriptor, manifest));
        } else {
            if runnable.is_some()
                || descriptor
                    .annotations
                    .contains_key("vnd.docker.reference.type")
            {
                return Err(OciArchiveError::Index);
            }
            if !descriptor
                .platform
                .as_ref()
                .is_some_and(|platform| model::platform(platform, profile))
            {
                return Err(OciArchiveError::Platform);
            }
            runnable = Some((descriptor, manifest));
        }
    }
    let (entry, manifest) = runnable.ok_or(OciArchiveError::Index)?;
    if manifest.artifact_type.is_some()
        || manifest.subject.is_some()
        || manifest.config.media_type != model::CONFIG
        || manifest.layers.is_empty()
    {
        return Err(OciArchiveError::Configuration);
    }
    let config: Configuration =
        model::parse(referenced(store, &manifest.config, &mut used)?, limits)?;
    model::configuration(&config, profile, manifest.layers.len())?;
    for layer in &manifest.layers {
        if layer.media_type != model::LAYER {
            return Err(OciArchiveError::Application);
        }
        referenced(store, layer, &mut used)?;
    }
    for (descriptor, attested) in &attestations {
        attestation(store, descriptor, attested, &entry, limits, &mut used)?;
    }
    if used.len() != store.blobs.len() {
        return Err(OciArchiveError::Descriptor);
    }
    Ok(Selected {
        descriptor: entry,
        manifest,
        config,
        attestations: attestations.len(),
    })
}
