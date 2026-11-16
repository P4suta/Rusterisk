use flate2::{write::GzEncoder, Compression};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io::{self, Write},
};
use xtask::server_distribution::{ServerOciProfile, ServerTarget};

pub const INDEX: &str = "application/vnd.oci.image.index.v1+json";
pub const MANIFEST: &str = "application/vnd.oci.image.manifest.v1+json";
pub const CONFIG: &str = "application/vnd.oci.image.config.v1+json";
pub const LAYER: &str = "application/vnd.oci.image.layer.v1.tar+gzip";
type ConfigBytesPatch = fn(Vec<u8>) -> io::Result<Vec<u8>>;

pub fn digest(bytes: &[u8]) -> String {
    let hex: String = Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("sha256:{hex}")
}

#[derive(Clone)]
pub struct Entry {
    pub path: String,
    pub bytes: Vec<u8>,
    pub kind: tar::EntryType,
    pub mode: u32,
    pub uid: u64,
    pub gid: u64,
}

impl Entry {
    pub fn file(path: &str, bytes: &[u8]) -> Self {
        Self {
            path: path.into(),
            bytes: bytes.to_vec(),
            kind: tar::EntryType::Regular,
            mode: 0o644,
            uid: 0,
            gid: 0,
        }
    }
    pub fn directory(path: &str, uid: u64) -> Self {
        Self {
            path: path.into(),
            bytes: Vec::new(),
            kind: tar::EntryType::Directory,
            mode: 0o755,
            uid,
            gid: uid,
        }
    }
}

pub fn tar(entries: &[Entry]) -> io::Result<Vec<u8>> {
    let mut archive = tar::Builder::new(Vec::new());
    for entry in entries {
        let mut header = tar::Header::new_ustar();
        if entry.path.len() > 99 {
            return Err(io::Error::other("fixture path too long"));
        }
        header.as_mut_bytes()[..entry.path.len()].copy_from_slice(entry.path.as_bytes());
        header.set_entry_type(entry.kind);
        header.set_mode(entry.mode);
        header.set_uid(entry.uid);
        header.set_gid(entry.gid);
        header.set_mtime(0);
        header.set_size(entry.bytes.len() as u64);
        header.set_cksum();
        archive.append(&header, entry.bytes.as_slice())?;
    }
    archive.into_inner()
}

pub fn gzip(bytes: &[u8]) -> io::Result<Vec<u8>> {
    let mut writer = GzEncoder::new(Vec::new(), Compression::default());
    writer.write_all(bytes)?;
    writer.finish()
}

fn blob(blobs: &mut BTreeMap<String, Vec<u8>>, media: &str, bytes: Vec<u8>) -> Value {
    let digest = digest(&bytes);
    let descriptor = json!({"mediaType": media, "digest": digest, "size": bytes.len()});
    blobs.insert(digest, bytes);
    descriptor
}

#[derive(Clone, Copy)]
pub enum Attestation {
    None,
    Legacy,
    Artifact,
}

pub struct Image {
    pub profile: ServerOciProfile,
    pub expected: Vec<(String, Vec<u8>)>,
    pub application: Vec<Entry>,
    pub config_patch: Option<fn(&mut Value)>,
    pub config_bytes_patch: Option<ConfigBytesPatch>,
    pub manifest_patch: Option<fn(&mut Value)>,
    pub index_patch: Option<fn(&mut Value)>,
    pub attestation_patch: Option<fn(&mut Value)>,
    pub nested: bool,
    pub attestation: Attestation,
    pub application_suffix: Vec<u8>,
}

impl Image {
    pub fn new(target: ServerTarget) -> io::Result<Self> {
        let profile = ServerOciProfile::new(target, "0.1.0", &"a".repeat(64))?;
        let expected: Vec<_> = target
            .required_files()
            .map(|path| (path.to_owned(), format!("candidate {path}\n").into_bytes()))
            .into();
        let mut application = [
            Entry::directory("opt/", 0),
            Entry::directory("opt/rusterisk/", 65532),
            Entry::directory("opt/rusterisk/bin/", 65532),
            Entry::directory("opt/rusterisk/config/", 65532),
            Entry::directory("opt/rusterisk/data/", 65532),
        ]
        .to_vec();
        for (path, bytes) in &expected {
            let mut entry = Entry::file(&format!("opt/rusterisk/{path}"), bytes);
            entry.uid = 65532;
            entry.gid = 65532;
            entry.mode = if path == target.binary_path() {
                0o755
            } else {
                0o644
            };
            application.push(entry);
        }
        Ok(Self {
            profile,
            expected,
            application,
            config_patch: None,
            config_bytes_patch: None,
            manifest_patch: None,
            index_patch: None,
            attestation_patch: None,
            nested: true,
            attestation: Attestation::Artifact,
            application_suffix: Vec::new(),
        })
    }
    pub fn files(&self) -> impl Iterator<Item = (&str, &[u8])> {
        self.expected
            .iter()
            .map(|(path, bytes)| (path.as_str(), bytes.as_slice()))
    }
    pub fn entries(&self) -> io::Result<Vec<Entry>> {
        let mut blobs = BTreeMap::new();
        let base = tar(&[Entry::file("etc/os-release", b"opaque base\n")])?;
        let application = tar(&self.application)?;
        let base_descriptor = blob(&mut blobs, LAYER, gzip(&base)?);
        let mut compressed_application = gzip(&application)?;
        compressed_application.extend_from_slice(&self.application_suffix);
        let app_descriptor = blob(&mut blobs, LAYER, compressed_application);
        let diff_ids = [digest(&base), digest(&application)];
        let entrypoint = ["/opt/rusterisk/bin/rusterisk"];
        let cmd = [
            "--config-dir",
            "/opt/rusterisk/config",
            "--data-dir",
            "/opt/rusterisk/data",
        ];
        let mut config = json!({
            "architecture": self.profile.platform().strip_prefix("linux/").ok_or_else(|| io::Error::other("fixture platform"))?, "os": "linux",
            "rootfs": {"type": "layers", "diff_ids": diff_ids},
            "config": {"User": "65532:65532", "Entrypoint": entrypoint, "Cmd": cmd, "WorkingDir": "/opt/rusterisk", "StopSignal": "SIGTERM",
                "ExposedPorts": {"8088/tcp":{}, "5060/udp":{}},
                "Labels": {"org.opencontainers.image.source": "https://github.com/P4suta/Rusterisk", "org.opencontainers.image.licenses": "MIT OR Apache-2.0", "org.opencontainers.image.version": self.profile.version(), "io.rusterisk.source.sha256": self.profile.source_digest()}}
        });
        if let Some(patch) = self.config_patch {
            patch(&mut config);
        }
        let config_bytes = serde_json::to_vec(&config)?;
        let config_bytes = match self.config_bytes_patch {
            Some(patch) => patch(config_bytes)?,
            None => config_bytes,
        };
        let config_descriptor = blob(&mut blobs, CONFIG, config_bytes);
        let layers = [base_descriptor, app_descriptor];
        let mut manifest = json!({"schemaVersion":2,"mediaType":MANIFEST,"config":config_descriptor,"layers":layers});
        if let Some(patch) = self.manifest_patch {
            patch(&mut manifest);
        }
        let mut runnable = blob(&mut blobs, MANIFEST, serde_json::to_vec(&manifest)?);
        runnable["platform"] = json!({"os":"linux", "architecture": config["architecture"]});
        let mut descriptors = [runnable.clone()].to_vec();
        match self.attestation {
            Attestation::None => {}
            Attestation::Legacy | Attestation::Artifact => {
                let subject = [
                    json!({"name":"candidate", "digest":{"sha256": runnable["digest"].as_str().ok_or_else(|| io::Error::other("fixture digest"))?.strip_prefix("sha256:")}}),
                ];
                let statement = json!({"_type":"https://in-toto.io/Statement/v1", "subject":subject, "predicateType":"https://slsa.dev/provenance/v1", "predicate":{}});
                let statement = blob(
                    &mut blobs,
                    "application/vnd.in-toto+json",
                    serde_json::to_vec(&statement)?,
                );
                let layers = [statement];
                let config = match self.attestation {
                    Attestation::Legacy => blob(&mut blobs, CONFIG, b"{}".to_vec()),
                    Attestation::Artifact => blob(
                        &mut blobs,
                        "application/vnd.oci.empty.v1+json",
                        b"{}".to_vec(),
                    ),
                    Attestation::None => return Err(io::Error::other("fixture attestation")),
                };
                let mut attestation =
                    json!({"schemaVersion":2,"mediaType":MANIFEST,"config":config,"layers":layers});
                if matches!(self.attestation, Attestation::Artifact) {
                    attestation["artifactType"] =
                        json!("application/vnd.docker.attestation.manifest.v1+json");
                    attestation["subject"] = runnable.clone();
                }
                if let Some(patch) = self.attestation_patch {
                    patch(&mut attestation);
                }
                let mut descriptor = blob(&mut blobs, MANIFEST, serde_json::to_vec(&attestation)?);
                descriptor["platform"] = json!({"os":"unknown","architecture":"unknown"});
                descriptor["annotations"] = json!({"vnd.docker.reference.type":"attestation-manifest","vnd.docker.reference.digest":runnable["digest"]});
                descriptors.push(descriptor);
            }
        }
        let mut index = json!({"schemaVersion":2,"mediaType":INDEX,"manifests":descriptors});
        if let Some(patch) = self.index_patch {
            patch(&mut index);
        }
        if self.nested {
            let descriptor = blob(&mut blobs, INDEX, serde_json::to_vec(&index)?);
            let manifests = [descriptor];
            index = json!({"schemaVersion":2,"mediaType":INDEX,"manifests":manifests});
        }
        let mut entries = [
            Entry::directory("blobs/", 0),
            Entry::directory("blobs/sha256/", 0),
            Entry::file("oci-layout", br#"{"imageLayoutVersion":"1.0.0"}"#),
            Entry::file("index.json", &serde_json::to_vec(&index)?),
        ]
        .to_vec();
        for (digest, bytes) in blobs {
            let hex = digest
                .strip_prefix("sha256:")
                .ok_or_else(|| io::Error::other("fixture digest"))?;
            entries.push(Entry::file(&format!("blobs/sha256/{hex}"), &bytes));
        }
        Ok(entries)
    }
    pub fn bytes(&self) -> io::Result<Vec<u8>> {
        tar(&self.entries()?)
    }
    pub fn expanded_bytes(&self) -> io::Result<u64> {
        Ok(
            (tar(&[Entry::file("etc/os-release", b"opaque base\n")])?.len()
                + tar(&self.application)?.len()) as u64,
        )
    }
}

pub fn empty_index_graph(image: &Image) -> io::Result<Vec<u8>> {
    let mut entries = image.entries()?;
    let mut blobs = BTreeMap::new();
    let manifests: [Value; 0] = [];
    let mut descriptor = blob(
        &mut blobs,
        INDEX,
        serde_json::to_vec(&json!({"schemaVersion":2,"mediaType":INDEX,"manifests":manifests}))?,
    );
    for _ in 0..3 {
        let manifests = std::iter::repeat_n(descriptor, 8).collect::<Vec<_>>();
        descriptor = blob(
            &mut blobs,
            INDEX,
            serde_json::to_vec(
                &json!({"schemaVersion":2,"mediaType":INDEX,"manifests":manifests}),
            )?,
        );
    }
    let index = entries
        .iter_mut()
        .find(|entry| entry.path == "index.json")
        .ok_or_else(|| io::Error::other("fixture index"))?;
    let mut value: Value = serde_json::from_slice(&index.bytes)?;
    value["manifests"]
        .as_array_mut()
        .ok_or_else(|| io::Error::other("fixture manifests"))?
        .push(descriptor);
    index.bytes = serde_json::to_vec(&value)?;
    for (digest, bytes) in blobs {
        let hex = digest
            .strip_prefix("sha256:")
            .ok_or_else(|| io::Error::other("fixture digest"))?;
        entries.push(Entry::file(&format!("blobs/sha256/{hex}"), &bytes));
    }
    tar(&entries)
}
