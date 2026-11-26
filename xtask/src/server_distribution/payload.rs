use super::{
    check_binary_target_header, BinaryTargetError, CandidateNameError, ServerCandidateName,
    ServerTarget,
};
use std::io;

#[derive(Debug)]
pub enum PayloadError {
    Io(io::Error),
    Manifest(toml::de::Error),
    Name(CandidateNameError),
    Binary(BinaryTargetError),
    Input(&'static str),
}

impl std::fmt::Display for PayloadError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => error.fmt(formatter),
            Self::Manifest(error) => error.fmt(formatter),
            Self::Name(error) => error.fmt(formatter),
            Self::Binary(error) => error.fmt(formatter),
            Self::Input(path) => write!(
                formatter,
                "server candidate requires bounded regular input: {path}"
            ),
        }
    }
}

impl std::error::Error for PayloadError {}
impl From<io::Error> for PayloadError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

struct File {
    path: &'static str,
    bytes: Vec<u8>,
}

fn input(
    path: &'static str,
    read: &mut impl FnMut(&str) -> io::Result<Vec<u8>>,
) -> Result<Vec<u8>, PayloadError> {
    let bytes = read(path)?;
    if bytes.is_empty()
        || bytes.len() > 65_536
        || std::str::from_utf8(&bytes).is_err()
        || bytes
            .iter()
            .any(|byte| byte.is_ascii_control() && !matches!(byte, b'\r' | b'\n' | b'\t'))
    {
        return Err(PayloadError::Input(path));
    }
    Ok(bytes)
}

fn file(
    path: &'static str,
    read: &mut impl FnMut(&str) -> io::Result<Vec<u8>>,
) -> Result<File, PayloadError> {
    Ok(File {
        path,
        bytes: input(path, read)?,
    })
}

pub struct ServerPayload {
    name: ServerCandidateName,
    files: [File; super::SERVER_FILE_COUNT],
}

impl ServerPayload {
    pub fn capture(
        target: ServerTarget,
        binary: Vec<u8>,
        cli: Vec<u8>,
        mut read: impl FnMut(&str) -> io::Result<Vec<u8>>,
    ) -> Result<Self, PayloadError> {
        for (path, bytes) in [(target.binary_path(), &binary), (target.cli_path(), &cli)] {
            if bytes.is_empty() || bytes.len() > 256 * 1024 * 1024 {
                return Err(PayloadError::Input(path));
            }
            check_binary_target_header(target, bytes).map_err(PayloadError::Binary)?;
        }
        let manifest = input("Cargo.toml", &mut read)?;
        let manifest: toml::Value = toml::from_str(
            std::str::from_utf8(&manifest).map_err(|_| PayloadError::Input("Cargo.toml"))?,
        )
        .map_err(PayloadError::Manifest)?;
        let version = manifest
            .get("workspace")
            .and_then(|workspace| workspace.get("package"))
            .and_then(|package| package.get("version"))
            .and_then(toml::Value::as_str)
            .ok_or(PayloadError::Input("workspace.package.version"))?;
        let name = ServerCandidateName::new(version, target).map_err(PayloadError::Name)?;
        let files = [
            File {
                path: target.binary_path(),
                bytes: binary,
            },
            File {
                path: target.cli_path(),
                bytes: cli,
            },
            File {
                path: "README.md",
                bytes: input("docs/server-distribution.md", &mut read)?,
            },
            file("LICENSE-MIT", &mut read)?,
            file("LICENSE-APACHE", &mut read)?,
            file("config/http.conf", &mut read)?,
            file("config/ari.conf", &mut read)?,
            file("config/pjsip.conf", &mut read)?,
            file("config/rtp.conf", &mut read)?,
            file("config/extensions.conf", &mut read)?,
        ];
        Ok(Self { name, files })
    }

    pub fn name(&self) -> &ServerCandidateName {
        &self.name
    }
    pub fn files(&self) -> impl Iterator<Item = (&str, &[u8])> {
        self.files
            .iter()
            .map(|file| (file.path, file.bytes.as_slice()))
    }
}
