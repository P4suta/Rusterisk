use super::{failure, TlsClientContext, TlsContext};
use crate::ControlError;
use rusterisk_config::TlsFiles;
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::rustls::pki_types::ServerName;

#[tokio::test]
async fn client_handshakes_require_a_trusted_certificate_and_matching_name(
) -> Result<(), ControlError> {
    let directory = tempfile::tempdir().map_err(failure)?;
    let identity = rcgen::generate_simple_self_signed(["localhost".into()]).map_err(failure)?;
    let unrelated = rcgen::generate_simple_self_signed(["localhost".into()]).map_err(failure)?;
    let cert_path = directory.path().join("certificate.pem");
    let key_path = directory.path().join("private-key.pem");
    let unrelated_path = directory.path().join("unrelated.pem");
    std::fs::write(&cert_path, identity.cert.pem()).map_err(failure)?;
    std::fs::write(&key_path, identity.signing_key.serialize_pem()).map_err(failure)?;
    std::fs::write(&unrelated_path, unrelated.cert.pem()).map_err(failure)?;
    let context = TlsContext::load(&TlsFiles {
        certificate: cert_path.to_string_lossy().into(),
        private_key: key_path.to_string_lossy().into(),
    })
    .await?;
    for (path, name, trusted) in [
        (&cert_path, "localhost", true),
        (&cert_path, "wrong.example", false),
        (&unrelated_path, "localhost", false),
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").await.map_err(failure)?;
        let address = listener.local_addr().map_err(failure)?;
        let server_context = context.clone();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.map_err(failure)?;
            server_context.accept(stream).await
        });
        let client = TlsClientContext::load(Some(path.to_string_lossy().as_ref()))?;
        let stream = TcpStream::connect(address).await.map_err(failure)?;
        let result = client
            .connect(stream, ServerName::try_from(name).map_err(failure)?)
            .await;
        assert_eq!(result.is_ok(), trusted, "{name}");
        drop(result);
        let accepted = server.await.map_err(failure)?;
        if trusted {
            assert!(accepted.is_ok());
        } else if let Err(error) = accepted {
            assert!(!error.to_string().is_empty());
        }
    }
    Ok(())
}
