use super::{failure, read};
use crate::ControlError;
use std::{path::Path, time::Duration};
use tokio::net::TcpStream;
use tokio_rustls::{
    client::TlsStream,
    rustls::{
        pki_types::{pem::PemObject, CertificateDer, ServerName},
        ClientConfig, RootCertStore,
    },
    TlsConnector,
};

#[derive(Clone)]
pub(crate) struct TlsClientContext(tokio_tungstenite::Connector);

fn client_connector(config: ::tokio_rustls::rustls::ClientConfig) -> tokio_tungstenite::Connector {
    #[expect(
        clippy::disallowed_types,
        reason = "docs/adr/confine-client-tls-sharing-to-immutable-library-config.md: Construct only immutable external TLS client configuration."
    )]
    let config = ::std::sync::Arc::<::tokio_rustls::rustls::ClientConfig>::new(config);
    tokio_tungstenite::Connector::Rustls(config)
}

pub(super) fn native_certificates(
    mut load: impl FnMut() -> rustls_native_certs::CertificateResult,
    mut wait: impl FnMut(),
) -> Result<Vec<CertificateDer<'static>>, ControlError> {
    let mut certificates = load();
    for _ in 0..2 {
        if certificates.errors.is_empty() {
            break;
        }
        wait();
        certificates = load();
    }
    if let Some(error) = certificates.errors.first() {
        let detail = match &error.kind {
            rustls_native_certs::ErrorKind::Io { inner, .. } => {
                format!("I/O {:?} ({:?})", inner.kind(), inner.raw_os_error())
            }
            rustls_native_certs::ErrorKind::Os(error) => {
                format!("Operating system: {error:?}")
            }
            rustls_native_certs::ErrorKind::Pem(error) => format!("PEM: {error}"),
            _ => "Unknown native trust error".into(),
        };
        return Err(ControlError::Adapter(format!("Native certificate authority loading failed ({} certificates, {} errors): {}: {detail}", certificates.certs.len(), certificates.errors.len(), error.context)));
    }
    if certificates.certs.len() > 1024
        || certificates
            .certs
            .iter()
            .map(|certificate| certificate.len())
            .sum::<usize>()
            > 4 * 1024 * 1024
    {
        return Err(ControlError::Capacity);
    }
    Ok(certificates.certs)
}

impl TlsClientContext {
    pub(crate) fn websocket_connector(&self) -> tokio_tungstenite::Connector {
        self.0.clone()
    }

    fn roots(ca_list_file: Option<&str>) -> Result<RootCertStore, ControlError> {
        let mut roots = RootCertStore::empty();
        match ca_list_file {
            Some(path) => {
                let bytes = read(Path::new(path), 65536)?;
                let mut count = 0;
                for certificate in CertificateDer::pem_slice_iter(&bytes) {
                    count += 1;
                    if count > 128 {
                        return Err(ControlError::Capacity);
                    }
                    roots.add(certificate.map_err(failure)?).map_err(failure)?;
                }
            }
            None => {
                let certificates =
                    native_certificates(rustls_native_certs::load_native_certs, || {
                        std::thread::sleep(Duration::from_millis(100))
                    })?;
                for certificate in certificates {
                    roots.add(certificate).map_err(failure)?;
                }
            }
        }
        if roots.is_empty() {
            return Err(ControlError::Invalid(
                "No certificate authorities configured",
            ));
        }
        Ok(roots)
    }

    pub(crate) fn load(ca_list_file: Option<&str>) -> Result<Self, ControlError> {
        let roots = Self::roots(ca_list_file)?;
        let config = ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        Ok(Self(client_connector(config)))
    }

    pub(crate) fn load_sip(ca_list_file: Option<&str>) -> Result<Self, ControlError> {
        let roots = Self::roots(ca_list_file)?;
        Ok(Self(client_connector(
            super::sip_domain::sip_client_configuration(roots),
        )))
    }

    pub(crate) async fn connect(
        &self,
        stream: TcpStream,
        name: ServerName<'static>,
    ) -> Result<TlsStream<TcpStream>, ControlError> {
        let connector = match self.0.clone() {
            tokio_tungstenite::Connector::Rustls(config) => TlsConnector::from(config),
            tokio_tungstenite::Connector::Plain => {
                return Err(ControlError::Invalid("Expected encrypted client connector"));
            }
            _ => return Err(ControlError::Unsupported),
        };
        tokio::time::timeout(Duration::from_secs(5), connector.connect(name, stream))
            .await
            .map_err(failure)?
            .map_err(failure)
    }
}
