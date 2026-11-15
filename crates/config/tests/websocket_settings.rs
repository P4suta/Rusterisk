use rusterisk_config::{Config, ConfigError, WebSocketControlFormat};

fn base() -> Result<Config, ConfigError> {
    Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=test-password\n",
        "",
        "",
        "",
    )
}

#[test]
fn media_configuration_uses_reference_sections_and_redacts_connection_credentials(
) -> Result<(), ConfigError> {
    let config = base()?.with_websocket_configuration("[global]\ncontrol_message_format=json\n", "[agent]\ntype=websocket_client\nconnection_type=per_call_config\nuri=wss://media.example/stream?token=private-token\nprotocols=media\nusername=private-username\npassword=private-password\ntls_enabled=yes\nverify_server_cert=yes\nverify_server_hostname=yes\nconnection_timeout=500\nreconnect_interval=500\nreconnect_attempts=4\n")?;
    assert_eq!(
        config.websocket.control_format,
        WebSocketControlFormat::Json
    );
    let client = &config.websocket.clients[0];
    assert_eq!(client.name, "agent");
    assert_eq!(
        client.uri.expose(),
        "wss://media.example/stream?token=private-token"
    );
    assert_eq!(client.connection_timeout_ms, 500);
    assert_eq!(client.reconnect_attempts, 4);
    let debug = format!("{config:?}");
    for secret in ["private-token", "private-username", "private-password"] {
        assert!(!debug.contains(secret));
    }
    Ok(())
}

#[test]
fn media_configuration_rejects_unknown_options_and_inconsistent_security_at_the_source_position(
) -> Result<(), ConfigError> {
    let prefix = "[agent]\ntype=websocket_client\nconnection_type=per_call_config\nuri=wss://media.example/stream\nprotocols=media\ntls_enabled=yes\n";
    for (option, line) in [
        ("unknown=value", 7),
        ("verify_server_cert=no", 7),
        ("verify_server_hostname=no", 7),
        ("password=unpaired", 7),
        ("connection_timeout=0", 7),
        ("reconnect_attempts=0", 7),
    ] {
        let result = base()?.with_websocket_configuration("", &format!("{prefix}{option}\n"));
        assert!(
            matches!(result, Err(ConfigError { file, line: actual, .. }) if file == "websocket_client.conf" && actual == line),
            "{option}"
        );
    }
    for uri in [
        "https://media.example",
        "wss://user:password@media.example",
        "wss://media.example/#fragment",
    ] {
        assert!(matches!(
            base()?.with_websocket_configuration(
                "",
                &prefix.replace("wss://media.example/stream", uri)
            ),
            Err(ConfigError { line: 4, .. })
        ));
    }
    assert!(
        matches!(base()?.with_websocket_configuration("[global]\ncontrol_message_format=invalid\n", ""), Err(ConfigError { file, line: 2, .. }) if file == "chan_websocket.conf")
    );
    Ok(())
}
