use super::{failure, TlsClientContext, TlsContext};
use crate::ControlError;
use rusterisk_config::TlsFiles;
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::rustls::pki_types::ServerName;

fn incomplete_trust(
    certificate: tokio_rustls::rustls::pki_types::CertificateDer<'static>,
) -> rustls_native_certs::CertificateResult {
    let mut result = rustls_native_certs::CertificateResult::default();
    result.certs.push(certificate);
    result.errors.push(rustls_native_certs::Error {
        context: "failed to load user trust settings",
        kind: rustls_native_certs::ErrorKind::Io {
            inner: std::io::ErrorKind::PermissionDenied.into(),
            path: "private-native-trust-path".into(),
        },
    });
    result
}

#[test]
fn incomplete_native_trust_is_discarded_before_a_complete_retry() -> Result<(), ControlError> {
    let discarded =
        rcgen::generate_simple_self_signed(["discarded.invalid".into()]).map_err(failure)?;
    let accepted =
        rcgen::generate_simple_self_signed(["accepted.invalid".into()]).map_err(failure)?;
    for failures in 0..=2 {
        let mut results = std::collections::VecDeque::with_capacity(3);
        for _ in 0..failures {
            results.push_back(incomplete_trust(discarded.cert.der().clone()));
        }
        let mut complete = rustls_native_certs::CertificateResult::default();
        complete.certs.push(accepted.cert.der().clone());
        results.push_back(complete);
        let (mut attempts, mut waits) = (0, 0);
        let certificates = super::client::native_certificates(
            || {
                attempts += 1;
                results.pop_front().unwrap_or_default()
            },
            || waits += 1,
        )?;
        assert_eq!(attempts, failures + 1);
        assert_eq!(waits, failures);
        assert_eq!(certificates, [accepted.cert.der().clone()]);
    }
    Ok(())
}

#[test]
fn persistent_native_failure_exhausts_three_reads_without_admitting_partial_roots(
) -> Result<(), ControlError> {
    let identity =
        rcgen::generate_simple_self_signed(["partial.invalid".into()]).map_err(failure)?;
    let (mut attempts, mut waits) = (0, 0);
    let result = super::client::native_certificates(
        || {
            attempts += 1;
            incomplete_trust(identity.cert.der().clone())
        },
        || waits += 1,
    );
    let Err(error) = result else {
        return Err(ControlError::Invalid(
            "Incomplete native trust was accepted",
        ));
    };
    assert_eq!(attempts, 3);
    assert_eq!(waits, 2);
    let diagnostic = error.to_string();
    assert!(diagnostic.contains("failed to load user trust settings"));
    assert!(diagnostic.contains("PermissionDenied"));
    assert!(!diagnostic.contains("private-native-trust-path"));
    Ok(())
}

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
