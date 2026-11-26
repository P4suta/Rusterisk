use super::{ExtractedServer, ServerCandidate, ServerOciProfile};
use std::{
    io::{self, Write},
    path::{Component, Path, PathBuf},
};

pub struct ServerOciContext<'a> {
    root: Option<PathBuf>,
    payload: Option<ExtractedServer>,
    profile: ServerOciProfile,
    candidate: &'a ServerCandidate,
}

fn directory(path: &Path, expected: &[(&str, bool)]) -> io::Result<()> {
    if !std::fs::symlink_metadata(path)?.is_dir() {
        return Err(io::Error::other("server image context directory changed"));
    }
    let mut count = 0;
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        count += 1;
        let (_, directory) = expected
            .iter()
            .find(|(name, _)| entry.file_name() == *name)
            .ok_or_else(|| io::Error::other("server image context has an unexpected entry"))?;
        let kind = entry.file_type()?;
        if count > expected.len()
            || (*directory && !kind.is_dir())
            || (!directory && !kind.is_file())
        {
            return Err(io::Error::other("server image context entry changed"));
        }
    }
    if count != expected.len() {
        return Err(io::Error::other("server image context entry is missing"));
    }
    Ok(())
}

fn combine(operation: io::Result<()>, cleanup: io::Result<()>) -> io::Result<()> {
    match (operation, cleanup) {
        (operation, Ok(())) => operation,
        (Ok(()), Err(error)) => Err(error),
        (Err(operation), Err(cleanup)) => Err(io::Error::other(format!(
            "{operation}; server image context cleanup: {cleanup}"
        ))),
    }
}

impl<'a> ServerOciContext<'a> {
    pub fn create(candidate: &'a ServerCandidate, root: &Path) -> io::Result<Self> {
        let profile = ServerOciProfile::new(
            candidate.name().target(),
            candidate.name().version(),
            candidate.source_digest(),
        )?;
        if !root.is_absolute()
            || root
                .to_str()
                .is_none_or(|path| path.len() > 4096 || path.chars().any(char::is_control))
            || root
                .components()
                .any(|part| matches!(part, Component::CurDir | Component::ParentDir))
        {
            return Err(io::Error::other(
                "server image contexts require bounded absolute paths",
            ));
        }
        let parent = root
            .parent()
            .ok_or_else(|| io::Error::other("server image context parent missing"))?;
        if std::fs::canonicalize(parent)? != parent {
            return Err(io::Error::other(
                "server image contexts require a physical parent",
            ));
        }
        std::fs::create_dir(root)?;
        let mut context = Self {
            root: Some(root.to_owned()),
            payload: None,
            profile,
            candidate,
        };
        let operation = (|| {
            context.payload = Some(candidate.extract(&root.join("payload"))?);
            std::fs::create_dir(root.join("payload/data"))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(
                    root.join("payload/data"),
                    std::fs::Permissions::from_mode(0o755),
                )?;
            }
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(root.join("Dockerfile"))?;
            file.write_all(context.profile.dockerfile().as_bytes())?;
            file.flush()?;
            context.check()
        })();
        match operation {
            Ok(()) => Ok(context),
            Err(operation) => match context.close() {
                Ok(()) => Err(operation),
                Err(cleanup) => Err(io::Error::other(format!(
                    "{operation}; server image context cleanup: {cleanup}"
                ))),
            },
        }
    }

    pub fn directory(&self) -> io::Result<&Path> {
        self.root
            .as_deref()
            .ok_or_else(|| io::Error::other("server image context ownership ended"))
    }
    pub fn profile(&self) -> &ServerOciProfile {
        &self.profile
    }
    pub fn check(&self) -> io::Result<()> {
        let root = self.directory()?;
        directory(root, &[("Dockerfile", false), ("payload", true)])?;
        let payload = root.join("payload");
        directory(
            &payload,
            &[
                ("bin", true),
                ("config", true),
                ("data", true),
                ("README.md", false),
                ("LICENSE-MIT", false),
                ("LICENSE-APACHE", false),
            ],
        )?;
        directory(
            &payload.join("bin"),
            &[("rusterisk", false), ("rusterisk-cli", false)],
        )?;
        directory(
            &payload.join("config"),
            &[
                ("http.conf", false),
                ("ari.conf", false),
                ("pjsip.conf", false),
                ("rtp.conf", false),
                ("extensions.conf", false),
            ],
        )?;
        directory(&payload.join("data"), &[])?;
        super::extraction::check_file(
            &root.join("Dockerfile"),
            self.profile.dockerfile().as_bytes(),
        )?;
        for (relative, bytes) in self.candidate.files() {
            let path = payload.join(relative);
            super::extraction::check_file(&path, bytes)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let expected = if self.profile.target().is_executable(relative) {
                    0o755
                } else {
                    0o644
                };
                if std::fs::symlink_metadata(path)?.permissions().mode() & 0o7777 != expected {
                    return Err(io::Error::other("server image payload file mode changed"));
                }
            }
        }
        Ok(())
    }
    pub fn close(mut self) -> io::Result<()> {
        self.finish()
    }
    fn finish(&mut self) -> io::Result<()> {
        let payload = match self.payload.take() {
            Some(payload) => payload.close(),
            None => Ok(()),
        };
        let root = self
            .root
            .take()
            .ok_or_else(|| io::Error::other("server image context ownership ended"))?;
        combine(payload, std::fs::remove_dir_all(root))
    }
}

impl Drop for ServerOciContext<'_> {
    fn drop(&mut self) {
        if self.root.is_some() {
            if let Err(error) = self.finish() {
                eprintln!("server image context fallback cleanup failed: {error}");
            }
        }
    }
}
