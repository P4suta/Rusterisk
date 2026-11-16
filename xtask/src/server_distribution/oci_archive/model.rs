use super::{OciArchiveError, OciArchiveLimit, OciArchiveLimits};
use crate::server_distribution::ServerOciProfile;
use serde::{
    de::{DeserializeOwned, MapAccess, Visitor},
    Deserialize, Deserializer,
};
use std::{collections::BTreeMap, fmt, marker::PhantomData};

pub(super) const INDEX: &str = "application/vnd.oci.image.index.v1+json";
pub(super) const MANIFEST: &str = "application/vnd.oci.image.manifest.v1+json";
pub(super) const CONFIG: &str = "application/vnd.oci.image.config.v1+json";
pub(super) const LAYER: &str = "application/vnd.oci.image.layer.v1.tar+gzip";

#[derive(Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(super) struct Descriptor {
    pub media_type: String,
    pub digest: String,
    pub size: u64,
    pub data: Option<String>,
    pub urls: Option<Vec<String>>,
    pub platform: Option<Platform>,
    #[serde(default, deserialize_with = "unique_map")]
    pub annotations: BTreeMap<String, String>,
}
#[derive(Deserialize, Clone)]
pub(super) struct Platform {
    pub os: String,
    pub architecture: String,
    pub variant: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Index {
    pub schema_version: u32,
    pub media_type: Option<String>,
    pub manifests: Vec<Descriptor>,
    #[serde(default, deserialize_with = "unique_map")]
    pub annotations: BTreeMap<String, String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Manifest {
    pub schema_version: u32,
    pub media_type: Option<String>,
    pub artifact_type: Option<String>,
    pub config: Descriptor,
    pub layers: Vec<Descriptor>,
    pub subject: Option<Descriptor>,
    #[serde(default, deserialize_with = "unique_map")]
    pub annotations: BTreeMap<String, String>,
}
#[derive(Deserialize)]
pub(super) struct Configuration {
    pub os: String,
    pub architecture: String,
    pub variant: Option<String>,
    pub config: Execution,
    pub rootfs: Rootfs,
}
#[derive(Deserialize)]
pub(super) struct Rootfs {
    #[serde(rename = "type")]
    pub kind: String,
    pub diff_ids: Vec<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(super) struct Execution {
    pub user: String,
    pub entrypoint: Vec<String>,
    pub cmd: Vec<String>,
    pub working_dir: String,
    #[serde(deserialize_with = "unique_map")]
    pub labels: BTreeMap<String, String>,
    pub stop_signal: String,
    #[serde(deserialize_with = "unique_map")]
    pub exposed_ports: BTreeMap<String, Empty>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Empty {}
#[derive(Deserialize)]
pub(super) struct Layout {
    #[serde(rename = "imageLayoutVersion")]
    pub version: String,
}

#[derive(Deserialize)]
pub(super) struct Statement {
    #[serde(rename = "_type")]
    pub kind: String,
    pub subject: Vec<StatementSubject>,
    #[serde(rename = "predicateType")]
    pub predicate_type: String,
}
#[derive(Deserialize)]
pub(super) struct StatementSubject {
    #[serde(deserialize_with = "unique_map")]
    pub digest: BTreeMap<String, String>,
}

fn unique_map<'de, D, T>(deserializer: D) -> Result<BTreeMap<String, T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    struct Unique<T>(PhantomData<T>);
    impl<'de, T: Deserialize<'de>> Visitor<'de> for Unique<T> {
        type Value = BTreeMap<String, T>;
        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("an OCI map without duplicate keys")
        }
        fn visit_map<M: MapAccess<'de>>(self, mut input: M) -> Result<Self::Value, M::Error> {
            let mut map = BTreeMap::new();
            while let Some((key, value)) = input.next_entry::<String, T>()? {
                if map.insert(key, value).is_some() {
                    return Err(serde::de::Error::custom("duplicate OCI metadata key"));
                }
            }
            Ok(map)
        }
    }
    deserializer.deserialize_map(Unique(PhantomData))
}

pub(super) fn parse<T: DeserializeOwned>(
    bytes: &[u8],
    limits: OciArchiveLimits,
) -> Result<T, OciArchiveError> {
    if bytes.len() as u64 > limits.json {
        return Err(OciArchiveError::Limit(OciArchiveLimit::Json));
    }
    serde_json::from_slice(bytes).map_err(OciArchiveError::from)
}
pub(super) fn sha256(value: &str) -> Option<&str> {
    let hex = value.strip_prefix("sha256:")?;
    (hex.len() == 64
        && hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f')))
    .then_some(hex)
}
pub(super) fn untagged(annotations: &BTreeMap<String, String>) -> bool {
    !annotations.contains_key("org.opencontainers.image.ref.name")
        && !annotations.contains_key("io.containerd.image.name")
}
pub(super) fn platform(platform: &Platform, profile: &ServerOciProfile) -> bool {
    platform.os == "linux"
        && profile.platform().strip_prefix("linux/") == Some(platform.architecture.as_str())
        && match platform.variant.as_deref() {
            None | Some("") => true,
            Some("v8") => platform.architecture == "arm64",
            Some(_) => false,
        }
}
pub(super) fn configuration(
    config: &Configuration,
    profile: &ServerOciProfile,
    layers: usize,
) -> Result<(), OciArchiveError> {
    let expected = Platform {
        os: config.os.clone(),
        architecture: config.architecture.clone(),
        variant: config.variant.clone(),
    };
    if !platform(&expected, profile) {
        return Err(OciArchiveError::Platform);
    }
    let execution = &config.config;
    if execution.user != "65532:65532"
        || execution.entrypoint != ["/opt/rusterisk/bin/rusterisk"]
        || execution.cmd
            != [
                "--config-dir",
                "/opt/rusterisk/config",
                "--data-dir",
                "/opt/rusterisk/data",
            ]
        || execution.working_dir != "/opt/rusterisk"
        || execution.stop_signal != "SIGTERM"
        || execution.exposed_ports.len() != 2
        || !execution.exposed_ports.contains_key("8088/tcp")
        || !execution.exposed_ports.contains_key("5060/udp")
        || config.rootfs.kind != "layers"
        || config.rootfs.diff_ids.len() != layers
        || config
            .rootfs
            .diff_ids
            .iter()
            .any(|value| sha256(value).is_none())
    {
        return Err(OciArchiveError::Configuration);
    }
    for (key, value) in [
        (
            "org.opencontainers.image.source",
            "https://github.com/P4suta/Rusterisk",
        ),
        ("org.opencontainers.image.licenses", "MIT OR Apache-2.0"),
        ("org.opencontainers.image.version", profile.version()),
        ("io.rusterisk.source.sha256", profile.source_digest()),
    ] {
        if execution.labels.get(key).map(String::as_str) != Some(value) {
            return Err(OciArchiveError::Configuration);
        }
    }
    Ok(())
}
