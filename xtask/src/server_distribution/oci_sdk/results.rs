use super::{
    docker::Observed,
    identity::{EngineImage, ExpectedImage},
    runtime::{container_present, ownership},
    OciEngineProfile, ServerCandidate, ServerOciContents, ServerOciRunPhase, ServerOciRuntimePlan,
};
use crate::server_distribution::{check_server_ready_log, native::read, ServerRuntimeConfig};
use std::{collections::BTreeSet, fs, io, net::SocketAddr, path::Path};

pub(crate) struct OciRuntimeInputs<'a> {
    pub identity: &'a str,
    pub http: SocketAddr,
    pub sip: SocketAddr,
    pub profile: OciEngineProfile,
    pub configuration: &'a Path,
}

struct Cursor<'a> {
    root: &'a Path,
    sequence: u8,
    files: BTreeSet<String>,
}

fn invalid() -> io::Error {
    io::Error::other("retained OCI runtime outputs differ from the completed native profile")
}

impl<'a> Cursor<'a> {
    fn new(root: &'a Path) -> Self {
        Self {
            root,
            sequence: 0,
            files: BTreeSet::new(),
        }
    }

    fn next(&mut self, label: &str) -> io::Result<Observed> {
        self.sequence = self
            .sequence
            .checked_add(1)
            .filter(|value| *value <= 128)
            .ok_or_else(invalid)?;
        let stem = format!("{:03}-{label}", self.sequence);
        let stdout = format!("{stem}.stdout.txt");
        let stderr = format!("{stem}.stderr.txt");
        let observed = Observed {
            stdout: read(&self.root.join(&stdout), 1024 * 1024)?,
            stderr: read(&self.root.join(&stderr), 1024 * 1024)?,
        };
        self.files.insert(stdout);
        self.files.insert(stderr);
        Ok(observed)
    }

    fn present(&mut self, name: &str) -> io::Result<bool> {
        let observed = self.next("containers")?;
        container_present(&observed.stdout, &observed.stderr, name)
    }

    fn owner(&mut self, image: &EngineImage<'_>, name: &str) -> io::Result<String> {
        let observed = self.next("ownership")?;
        if !observed.stderr.is_empty() {
            return Err(invalid());
        }
        ownership(&observed.stdout, image, name)
    }

    fn finish(self) -> io::Result<()> {
        for entry in fs::read_dir(self.root)? {
            let name = entry?.file_name();
            let name = name.to_str().ok_or_else(invalid)?;
            if (name.ends_with(".stdout.txt") || name.ends_with(".stderr.txt"))
                && !self.files.contains(name)
            {
                return Err(invalid());
            }
        }
        Ok(())
    }
}

fn configuration(
    server: &ServerCandidate,
    root: &Path,
    inputs: &OciRuntimeInputs<'_>,
) -> io::Result<()> {
    let expected = ServerRuntimeConfig::capture(inputs.http, inputs.sip, |path| {
        server
            .files()
            .find(|(name, _)| *name == path)
            .map(|(_, bytes)| bytes.to_vec())
            .ok_or_else(invalid)
    })?;
    let mut names = BTreeSet::new();
    for (path, bytes) in expected.files() {
        let name = path.strip_prefix("config/").ok_or_else(invalid)?;
        if read(&root.join(name), 65_536)? != bytes {
            return Err(invalid());
        }
        names.insert(name);
    }
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if !entry.file_type()?.is_file()
            || !entry
                .file_name()
                .to_str()
                .is_some_and(|name| names.remove(name))
        {
            return Err(invalid());
        }
    }
    if !names.is_empty() {
        return Err(invalid());
    }
    Ok(())
}

fn serve(
    cursor: &mut Cursor<'_>,
    plan: &ServerOciRuntimePlan,
    image: &EngineImage<'_>,
    http: SocketAddr,
) -> io::Result<String> {
    let name = plan.container(ServerOciRunPhase::Serve);
    if cursor.present(&name)? {
        return Err(invalid());
    }
    let observed = cursor.next("serve")?;
    if !observed.stderr.is_empty() || !check_server_ready_log(&observed.stdout, http)? {
        return Err(invalid());
    }
    let id = cursor.owner(image, &name)?;
    cursor.next("stop")?.exact(format!("{id}\n").as_bytes())?;
    Ok(id)
}

pub(crate) fn check_oci_runtime_outputs(
    server: &ServerCandidate,
    checked: &ServerOciContents,
    root: &Path,
    inputs: &OciRuntimeInputs<'_>,
) -> io::Result<()> {
    for (name, value) in [
        ("CONFIG.sha256", checked.config_digest()),
        ("MANIFEST.sha256", checked.manifest_digest()),
        ("SOURCE.sha256", server.source_digest()),
    ] {
        if read(&root.join(name), 80)? != format!("{value}\n").as_bytes() {
            return Err(invalid());
        }
    }
    configuration(server, &root.join("runtime-config"), inputs)?;
    let initial = ServerOciRuntimePlan::new(
        inputs.identity,
        checked.config_digest(),
        inputs.configuration,
    )?;
    let mut cursor = Cursor::new(root);
    for phase in ServerOciRunPhase::ALL {
        if cursor.present(&initial.container(phase))? {
            return Err(invalid());
        }
    }
    let loaded = cursor.next("load")?;
    let image = EngineImage::loaded(
        &loaded.stdout,
        ExpectedImage {
            configuration: checked.config_digest(),
            manifest: checked.manifest_digest(),
            manifest_size: checked.manifest_size(),
            target: server.name().target(),
        },
    )?;
    if image.profile() != inputs.profile
        || read(&root.join("IMAGE.id"), 80)? != format!("{}\n", image.id()).as_bytes()
    {
        return Err(invalid());
    }
    let inspected = cursor.next("image")?;
    if !inspected.stderr.is_empty() {
        return Err(invalid());
    }
    image.check(&inspected.stdout)?;
    let plan = ServerOciRuntimePlan::new(inputs.identity, image.id(), inputs.configuration)?;
    cursor
        .next("version")?
        .exact(format!("rusterisk {}\n", server.name().version()).as_bytes())?;
    for label in ["packaged-config", "runtime-config"] {
        cursor.next(label)?.exact(b"Configuration valid.\n")?;
    }
    let first = serve(&mut cursor, &plan, &image, inputs.http)?;
    let second = serve(&mut cursor, &plan, &image, inputs.http)?;
    if first == second {
        return Err(invalid());
    }
    for phase in ServerOciRunPhase::ALL.into_iter().rev() {
        let name = plan.container(phase);
        if cursor.present(&name)? {
            let id = cursor.owner(&image, &name)?;
            cursor.next("remove")?.exact(format!("{id}\n").as_bytes())?;
            if cursor.present(&name)? {
                return Err(invalid());
            }
        }
    }
    cursor.finish()
}
