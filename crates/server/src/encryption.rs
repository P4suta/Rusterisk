use crate::ControlError;
use axum_server::accept::Accept;
use axum_server::tls_rustls::{RustlsAcceptor, RustlsConfig};
use rusterisk_config::TlsFiles;
use std::io::Read;
use std::path::Path;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio_rustls::server::TlsStream;
mod client;
mod sip_domain;
pub(crate) use client::TlsClientContext;
#[cfg(test)]
mod client_tests;

#[derive(Clone)]
pub(crate) struct TlsContext(RustlsAcceptor);

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}

fn check_material(metadata: &std::fs::Metadata, limit: usize) -> Result<(), ControlError> {
    if !metadata.is_file() {
        return Err(ControlError::Invalid(
            "Encryption material must be a regular file",
        ));
    }
    if metadata.len() == 0 || metadata.len() > limit as u64 {
        return Err(ControlError::Invalid(
            "Encryption material exceeds capacity",
        ));
    }
    Ok(())
}

fn read(path: &Path, limit: usize) -> Result<Vec<u8>, ControlError> {
    check_material(&std::fs::metadata(path).map_err(failure)?, limit)?;
    let file = std::fs::File::open(path).map_err(failure)?;
    check_material(&file.metadata().map_err(failure)?, limit)?;
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(failure)?;
    if bytes.is_empty() || bytes.len() > limit {
        return Err(ControlError::Invalid(
            "Encryption material exceeds capacity",
        ));
    }
    Ok(bytes)
}

impl TlsContext {
    pub async fn load(files: &TlsFiles) -> Result<Self, ControlError> {
        let certificate = read(Path::new(&files.certificate), 65536)?;
        let private_key = read(Path::new(&files.private_key), 16384)?;
        let config = RustlsConfig::from_pem(certificate, private_key)
            .await
            .map_err(failure)?;
        Ok(Self(
            RustlsAcceptor::new(config).handshake_timeout(Duration::from_secs(5)),
        ))
    }

    pub async fn accept(&self, stream: TcpStream) -> Result<TlsStream<TcpStream>, ControlError> {
        let (stream, ()) = self.0.accept(stream, ()).await.map_err(failure)?;
        Ok(stream)
    }
}
