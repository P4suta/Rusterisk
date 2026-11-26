use super::{invalid, Release};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    io,
};

#[derive(Clone, Deserialize)]
pub(super) struct Asset {
    pub name: String,
    pub state: String,
    pub digest: Option<String>,
}

pub(super) fn missing(
    expected: &BTreeMap<String, String>,
    assets: &[Asset],
) -> io::Result<Vec<String>> {
    let mut existing = BTreeSet::new();
    for asset in assets {
        let digest = asset
            .digest
            .as_deref()
            .and_then(|s| s.strip_prefix("sha256:"));
        if asset.state != "uploaded"
            || !existing.insert(&asset.name)
            || digest.is_none()
            || expected.get(&asset.name).map(String::as_str) != digest
        {
            return Err(invalid(format!(
                "unexpected or conflicting GitHub asset {}",
                asset.name
            )));
        }
    }
    Ok(expected
        .keys()
        .filter(|name| !existing.contains(name))
        .cloned()
        .collect())
}

pub(super) fn verify_tag(release: &Release<'_>, tag: &serde_json::Value) -> io::Result<()> {
    if tag["tag"] != format!("v{}", release.version)
        || tag.pointer("/object/type").and_then(|v| v.as_str()) != Some("commit")
        || tag.pointer("/object/sha").and_then(|v| v.as_str()) != Some(release.source)
        || tag
            .pointer("/verification/verified")
            .and_then(|v| v.as_bool())
            != Some(true)
    {
        return Err(invalid(
            "signed tag does not select the verified release commit",
        ));
    }
    Ok(())
}
