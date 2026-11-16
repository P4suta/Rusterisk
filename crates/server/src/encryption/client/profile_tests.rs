use super::{client_connector, failure, TlsClientContext};
use crate::ControlError;
use tokio_rustls::rustls::{
    pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer},
    ClientConfig, RootCertStore,
};

fn context(
    root: &rcgen::CertifiedKey<rcgen::KeyPair>,
    client: Option<&rcgen::CertifiedKey<rcgen::KeyPair>>,
) -> Result<TlsClientContext, ControlError> {
    let mut roots = RootCertStore::empty();
    roots.add(root.cert.der().clone()).map_err(failure)?;
    let builder = ClientConfig::builder().with_root_certificates(roots);
    let config = match client {
        None => builder.with_no_client_auth(),
        Some(client) => builder
            .with_client_auth_cert(
                std::iter::once(client.cert.der().clone()).collect(),
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(client.signing_key.serialize_der())),
            )
            .map_err(failure)?,
    };
    Ok(TlsClientContext(client_connector(config)))
}

#[test]
fn tls_profile_identity_matches_only_clones_of_one_immutable_configuration(
) -> Result<(), ControlError> {
    let root = rcgen::generate_simple_self_signed(["provider.test".into()]).map_err(failure)?;
    let first = context(&root, None)?;
    let cloned = first.clone();
    let rebuilt = context(&root, None)?;
    let plain = TlsClientContext(tokio_tungstenite::Connector::Plain);
    assert!(first.same_profile(&cloned));
    assert!(!first.same_profile(&rebuilt));
    assert!(!first.same_profile(&plain));
    assert!(!plain.same_profile(&plain));
    Ok(())
}

#[test]
fn changed_trust_roots_or_client_identity_never_share_a_tls_profile() -> Result<(), ControlError> {
    let first_root = rcgen::generate_simple_self_signed(["first.test".into()]).map_err(failure)?;
    let second_root =
        rcgen::generate_simple_self_signed(["second.test".into()]).map_err(failure)?;
    let client = rcgen::generate_simple_self_signed(["client.test".into()]).map_err(failure)?;
    let original = context(&first_root, None)?;
    let changed_roots = context(&second_root, None)?;
    let changed_identity = context(&first_root, Some(&client))?;
    assert!(!original.same_profile(&changed_roots));
    assert!(!original.same_profile(&changed_identity));
    Ok(())
}
