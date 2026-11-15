use super::{authority, dns, failure, issue, parameters, uri, ControlError};
use crate::encryption::{TlsClientContext, TlsContext};
use rusterisk_config::TlsFiles;
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::rustls::pki_types::ServerName;

enum NameKind {
    SipUri,
    Wildcard,
}
enum Profile {
    Sip,
    Web,
}

#[tokio::test]
async fn sip_and_web_clients_apply_their_own_certificate_identity_rules() -> Result<(), ControlError>
{
    for kind in [NameKind::SipUri, NameKind::Wildcard] {
        let directory = tempfile::tempdir().map_err(failure)?;
        let authority = authority()?;
        let (name, identity) = match kind {
            NameKind::SipUri => (
                "localhost",
                issue(
                    &authority,
                    &parameters(std::iter::once(uri("sip:localhost")?)),
                )?,
            ),
            NameKind::Wildcard => (
                "phone.example.test",
                issue(
                    &authority,
                    &parameters(std::iter::once(dns("*.example.test")?)),
                )?,
            ),
        };
        let certificate = directory.path().join("certificate.pem");
        let private_key = directory.path().join("private-key.pem");
        let root = directory.path().join("authority.pem");
        std::fs::write(&certificate, identity.cert.pem()).map_err(failure)?;
        std::fs::write(&private_key, identity.signing_key.serialize_pem()).map_err(failure)?;
        std::fs::write(&root, authority.pem()).map_err(failure)?;
        let server_context = TlsContext::load(&TlsFiles {
            certificate: certificate.to_string_lossy().into(),
            private_key: private_key.to_string_lossy().into(),
        })
        .await?;
        for profile in [Profile::Sip, Profile::Web] {
            let (client, accepted) = match profile {
                Profile::Sip => (
                    TlsClientContext::load_sip(Some(root.to_string_lossy().as_ref()))?,
                    matches!(kind, NameKind::SipUri),
                ),
                Profile::Web => (
                    TlsClientContext::load(Some(root.to_string_lossy().as_ref()))?,
                    matches!(kind, NameKind::Wildcard),
                ),
            };
            let listener = TcpListener::bind("127.0.0.1:0").await.map_err(failure)?;
            let address = listener.local_addr().map_err(failure)?;
            let context = server_context.clone();
            let server = tokio::spawn(async move {
                let (socket, _) = listener.accept().await.map_err(failure)?;
                context.accept(socket).await
            });
            let socket = TcpStream::connect(address).await.map_err(failure)?;
            let result = client
                .connect(socket, ServerName::try_from(name).map_err(failure)?)
                .await;
            assert_eq!(result.is_ok(), accepted);
            drop(result);
            let peer = server.await.map_err(failure)?;
            if accepted {
                assert!(peer.is_ok());
            }
            drop(peer);
        }
    }
    Ok(())
}
