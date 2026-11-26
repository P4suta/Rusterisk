use super::{ServerCandidate, ServerTarget};
use std::{
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

pub struct ExtractedServer {
    root: Option<PathBuf>,
    target: ServerTarget,
}

fn write_new(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(bytes)?;
    file.flush()
}

pub(super) fn check_file(path: &Path, expected: &[u8]) -> io::Result<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.len() != expected.len() as u64 {
        return Err(io::Error::other("extracted server file changed"));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(expected.len() as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes != expected {
        return Err(io::Error::other("extracted server bytes changed"));
    }
    Ok(())
}

impl ServerCandidate {
    pub fn extract(&self, destination: &Path) -> io::Result<ExtractedServer> {
        self.extract_with(destination, write_new)
    }

    pub fn extract_with(
        &self,
        destination: &Path,
        mut write: impl FnMut(&Path, &[u8]) -> io::Result<()>,
    ) -> io::Result<ExtractedServer> {
        std::fs::create_dir(destination)?;
        let extracted = ExtractedServer {
            root: Some(destination.to_owned()),
            target: self.name().target(),
        };
        let operation = (|| {
            for directory in ["bin", "config"] {
                std::fs::create_dir(destination.join(directory))?;
            }
            for (relative, bytes) in self.files() {
                let path = destination.join(relative);
                let parent = path
                    .parent()
                    .ok_or_else(|| io::Error::other("extracted server file parent missing"))?;
                if !std::fs::symlink_metadata(parent)?.is_dir() {
                    return Err(io::Error::other("extracted server directory changed"));
                }
                write(&path, bytes)?;
                if !std::fs::symlink_metadata(parent)?.is_dir() {
                    return Err(io::Error::other("extracted server directory changed"));
                }
                check_file(&path, bytes)?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(
                        &path,
                        std::fs::Permissions::from_mode(
                            if self.name().target().is_executable(relative) {
                                0o755
                            } else {
                                0o644
                            },
                        ),
                    )?;
                }
            }
            Ok(())
        })();
        match operation {
            Ok(()) => Ok(extracted),
            Err(operation) => match extracted.close() {
                Ok(()) => Err(operation),
                Err(cleanup) => Err(io::Error::other(format!(
                    "{operation}; server extraction cleanup failed: {cleanup}"
                ))),
            },
        }
    }
}

impl ExtractedServer {
    pub fn directory(&self) -> io::Result<&Path> {
        self.root
            .as_deref()
            .ok_or_else(|| io::Error::other("server extraction ownership ended"))
    }

    pub fn executable(&self) -> io::Result<PathBuf> {
        Ok(self.directory()?.join(self.target.binary_path()))
    }

    pub fn close(mut self) -> io::Result<()> {
        let root = self
            .root
            .take()
            .ok_or_else(|| io::Error::other("server extraction ownership ended"))?;
        std::fs::remove_dir_all(root)
    }
}

impl Drop for ExtractedServer {
    fn drop(&mut self) {
        if let Some(root) = self.root.take() {
            if let Err(error) = std::fs::remove_dir_all(root) {
                eprintln!("server extraction fallback cleanup failed: {error}");
            }
        }
    }
}
