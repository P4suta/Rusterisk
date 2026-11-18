use std::{
    io,
    path::{Path, PathBuf},
};

pub(super) struct BuildDirectory {
    root: Option<PathBuf>,
    identity: String,
}

impl BuildDirectory {
    pub(super) fn reserve(parent: &Path, identity: &str) -> io::Result<Self> {
        if !crate::reference::valid_identity(identity)
            || identity.len() > 61
            || !parent.is_absolute()
            || std::fs::canonicalize(parent)? != parent
        {
            return Err(io::Error::other(
                "server builds require a bounded identity and physical temporary parent",
            ));
        }
        for sequence in 0..32 {
            let identity = format!("{identity}-{sequence}");
            let root = parent.join(format!("rsk-{:x}-{sequence:x}", std::process::id()));
            match std::fs::create_dir(&root) {
                Ok(()) => {
                    return Ok(Self {
                        root: Some(root),
                        identity,
                    })
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => {
                    return Err(io::Error::new(
                        error.kind(),
                        format!(
                            "cannot reserve server build directory {}: {error}",
                            root.display()
                        ),
                    ))
                }
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "server build directory reservations are exhausted",
        ))
    }

    pub(super) fn path(&self) -> io::Result<&Path> {
        self.root
            .as_deref()
            .ok_or_else(|| io::Error::other("server build directory ownership ended"))
    }

    pub(super) fn identity(&self) -> &str {
        &self.identity
    }

    pub(super) fn close(mut self) -> io::Result<()> {
        self.finish()
    }

    fn finish(&mut self) -> io::Result<()> {
        let root = self
            .root
            .take()
            .ok_or_else(|| io::Error::other("server build directory ownership ended"))?;
        std::fs::remove_dir_all(&root).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!(
                    "cannot remove server build directory {}: {error}",
                    root.display()
                ),
            )
        })
    }
}

impl Drop for BuildDirectory {
    fn drop(&mut self) {
        if self.root.is_some() {
            if let Err(error) = self.finish() {
                eprintln!("server build directory fallback cleanup failed: {error}");
            }
        }
    }
}
