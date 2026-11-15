use rusterisk_config::Config;
use rusterisk_server::{runtime::Server, ControlError};

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}

#[tokio::test]
async fn configuration_validation_loads_and_bounds_outbound_signaling_trust_roots(
) -> Result<(), ControlError> {
    let directory = tempfile::tempdir().map_err(failure)?;
    let identity = rcgen::generate_simple_self_signed(["localhost".into()]).map_err(failure)?;
    let certificate = directory.path().join("certificate.pem");
    let private_key = directory.path().join("private-key.pem");
    let roots = directory.path().join("roots.pem");
    std::fs::write(&certificate, identity.cert.pem()).map_err(failure)?;
    std::fs::write(&private_key, identity.signing_key.serialize_pem()).map_err(failure)?;
    let config = Config::parse("[general]\nbindaddr=127.0.0.1\n", "[app]\ntype=user\npassword=test\n", &format!("[secure]\ntype=transport\nprotocol=tls\nbind=127.0.0.1:0\ncert_file={}\npriv_key_file={}\nca_list_file={}\n", certificate.display(), private_key.display(), roots.display()), "", "").map_err(failure)?;
    assert!(Server::validate(&config).await.is_err());
    for bytes in [Vec::new(), b"invalid roots".to_vec(), vec![b'x'; 65537]] {
        std::fs::write(&roots, bytes).map_err(failure)?;
        assert!(Server::validate(&config).await.is_err());
    }
    std::fs::write(&roots, identity.cert.pem()).map_err(failure)?;
    Server::validate(&config).await
}
