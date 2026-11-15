use rusterisk_config::{Config, ConfigError};

fn parse(transport: &str) -> Result<Config, ConfigError> {
    Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=test\n",
        transport,
        "",
        "",
    )
}

#[test]
fn tls_transport_accepts_an_optional_certificate_authority_list() -> Result<(), ConfigError> {
    let source = "[secure]\ntype=transport\nprotocol=tls\nbind=127.0.0.1:5061\ncert_file=certificate.pem\npriv_key_file=private-key.pem\nca_list_file=roots.pem\n";
    let config = parse(source)?;
    assert_eq!(
        config.transports[0].ca_list_file.as_deref(),
        Some("roots.pem")
    );
    assert_eq!(
        parse(&source.replace("ca_list_file=roots.pem\n", ""))?.transports[0].ca_list_file,
        None
    );
    Ok(())
}

#[test]
fn unencrypted_transports_reject_tls_material_with_the_option_position() {
    for key in ["ca_list_file", "cert_file", "priv_key_file"] {
        let source = format!(
            "[plain]\ntype=transport\nprotocol=tcp\nbind=127.0.0.1:5060\n{key}=certificate.pem\n"
        );
        let error = parse(&source);
        assert!(
            matches!(error, Err(ConfigError { line: 5, .. })),
            "{error:?}"
        );
    }
}
