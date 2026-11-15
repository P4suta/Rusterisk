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

impl TlsClientContext {
    pub(crate) fn load(ca_list_file: Option<&str>) -> Result<Self, ControlError> {
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
                let certificates = rustls_native_certs::load_native_certs();
                if !certificates.errors.is_empty() {
                    return Err(ControlError::Invalid(
                        "Native certificate authority loading failed",
                    ));
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
                for certificate in certificates.certs {
                    roots.add(certificate).map_err(failure)?;
                }
            }
        }
        if roots.is_empty() {
            return Err(ControlError::Invalid(
                "No certificate authorities configured",
            ));
        }
        let config = ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        Ok(Self(client_connector(config)))
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
