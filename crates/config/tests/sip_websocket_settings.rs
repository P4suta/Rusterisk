use rusterisk_config::{Config, ConfigError, Protocol, Reload};

const ARI: &str = "[app]\ntype=user\npassword=test\n";
const HTTP: &str = "[general]\nbindaddr=127.0.0.1\nbindport=8088\n";
const TLS: &str = "[general]\nbindaddr=127.0.0.1\ntlsenable=yes\ntlsbindaddr=127.0.0.1:8089\ntlscertfile=web-certificate.pem\ntlsprivatekey=web-key.pem\n";

#[test]
fn websocket_transports_use_the_http_listener_and_its_encryption() -> Result<(), ConfigError> {
    for (http, protocol, expected) in [(HTTP, "ws", Protocol::Ws), (TLS, "wss", Protocol::Wss)] {
        for bind in ["0.0.0.0", "[::]:5060", "127.0.0.1:0"] {
            let config = Config::parse(
                http,
                ARI,
                &format!("[browser]\ntype=transport\nprotocol={protocol}\nbind={bind}\n"),
                "",
                "",
            )?;
            assert_eq!(config.transports[0].protocol, expected);
            assert_eq!(config.transports[0].bind, config.http.bind);
            assert!(config.transports[0].tls.is_none());
        }
    }
    Ok(())
}

#[test]
fn websocket_transport_errors_identify_the_requested_scheme_or_tls_option() {
    for (http, protocol) in [(HTTP, "wss"), (TLS, "ws")] {
        let error = Config::parse(
            http,
            ARI,
            &format!("[browser]\ntype=transport\nprotocol={protocol}\nbind=0.0.0.0\n"),
            "",
            "",
        );
        assert!(
            matches!(error, Err(ConfigError { line: 3, .. })),
            "{error:?}"
        );
    }
    for key in ["cert_file", "priv_key_file", "ca_list_file"] {
        let error = Config::parse(
            TLS,
            ARI,
            &format!("[browser]\ntype=transport\nprotocol=wss\nbind=0.0.0.0\n{key}=material.pem\n"),
            "",
            "",
        );
        assert!(
            matches!(error, Err(ConfigError { line: 5, .. })),
            "{error:?}"
        );
    }
}

#[test]
fn ambiguous_websocket_transport_metadata_is_rejected_before_binding() {
    let error = Config::parse(
        HTTP,
        ARI,
        "[one]\ntype=transport\nprotocol=ws\nbind=0.0.0.0\n[two]\ntype=transport\nprotocol=ws\nbind=127.0.0.1\n",
        "",
        "",
    );
    assert!(
        matches!(error, Err(ConfigError { line: 7, .. })),
        "{error:?}"
    );
}

#[test]
fn websocket_origin_policy_replacement_requires_listener_restart() -> Result<(), ConfigError> {
    let source =
        "[general]\nallowed_origins=https://browser.example\n[app]\ntype=user\npassword=test\n";
    let sip = "[browser]\ntype=transport\nprotocol=ws\nbind=0.0.0.0\n";
    let current = Config::parse(HTTP, source, sip, "", "")?;
    let replacement = Config::parse(
        HTTP,
        &source.replace("browser.example", "replacement.example"),
        sip,
        "",
        "",
    )?;
    assert_eq!(current.reload(&replacement), Reload::RestartRequired);
    let current = Config::parse(HTTP, source, "", "", "")?;
    let replacement = Config::parse(
        HTTP,
        &source.replace("browser.example", "replacement.example"),
        "",
        "",
        "",
    )?;
    assert_eq!(current.reload(&replacement), Reload::Apply);
    Ok(())
}
