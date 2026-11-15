use rusterisk_config::{Codec, Config, ConfigError, Dtmf, Protocol};

#[test]
fn browser_example_configuration_keeps_services_local_and_matches_registered_addresses_to_aors(
) -> Result<(), ConfigError> {
    let root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/browser/config");
    let config = Config::load(&root)?;
    assert!(config.http.bind.ip().is_loopback());
    assert_eq!(config.http.bind.port(), 8088);
    assert_eq!(config.origins, ["http://127.0.0.1:3000"]);
    assert_eq!(config.transports.len(), 1);
    assert_eq!(config.transports[0].protocol, Protocol::Ws);
    assert!(config.transports[0].bind.ip().is_loopback());
    assert!(config
        .users
        .iter()
        .any(|user| user.name == "api" && !user.read_only));
    for name in ["alice", "bob"] {
        assert!(config
            .aors
            .iter()
            .any(|aor| aor.name == name && aor.max_contacts == 1 && aor.contact.is_none()));
        let endpoint = config
            .endpoints
            .iter()
            .find(|endpoint| endpoint.name == format!("{name}-endpoint"));
        assert!(endpoint.is_some());
        if let Some(endpoint) = endpoint {
            assert_eq!(endpoint.aors, [name]);
            assert_eq!(endpoint.transport, "browser");
            assert!(endpoint.webrtc);
            assert_eq!(endpoint.codecs, [Codec::Pcmu]);
            assert_eq!(endpoint.dtmf, Dtmf::Info);
            assert_eq!(endpoint.context, "internal");
        }
        assert!(config.auth.iter().any(|auth| auth.username == name));
    }
    assert!(config
        .dialplan
        .steps
        .iter()
        .any(|step| step.context == "internal" && step.extension == "100"));
    assert!(!format!("{config:?}").contains("test-password"));
    Ok(())
}
