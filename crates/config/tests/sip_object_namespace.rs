use rusterisk_config::{Config, ConfigError, Ini};

fn parse(source: &str) -> Result<Config, ConfigError> {
    Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[application]\ntype=user\npassword=sample\n",
        source,
        "",
        "",
    )
}

#[test]
fn signaling_objects_share_identifiers_only_across_distinct_types() -> Result<(), ConfigError> {
    let config = parse("[transport]\ntype=transport\nprotocol=udp\nbind=127.0.0.1:5060\n[phone]\ntype=auth\nusername=device\npassword=sample\n[phone]\ntype=aor\nmax_contacts=1\n[phone]\ntype=endpoint\ntransport=transport\nauth=phone\naors=phone\n")?;
    assert_eq!(config.endpoints[0].name, "phone");
    assert_eq!(config.endpoints[0].auth, config.auth[0].name);
    assert_eq!(config.endpoints[0].aors, [config.aors[0].name.as_str()]);
    assert_eq!(config.auth[0].username, "device");
    Ok(())
}

#[test]
fn duplicate_typed_objects_and_missing_types_keep_their_source_positions() {
    for (source, line) in [
        ("[phone]\ntype=aor\n[phone]\ntype=aor\n", 3),
        ("[phone]\ntype=auth\nusername=phone\npassword=sample\n[phone]\ntype=auth\nusername=other\npassword=sample\n", 5),
        ("[phone]\ntype=aor\n[phone]\nmax_contacts=1\n", 3),
        ("[phone]\ntype=aor\n[phone]\ntype=unknown\n", 3),
    ] {
        let result = parse(source);
        assert!(matches!(result, Err(ConfigError { file, line: actual, .. }) if file == "pjsip.conf" && actual == line));
    }
}

#[test]
fn general_settings_and_management_users_keep_unique_names() {
    let source = "[phone]\ntype=aor\n[phone]\ntype=endpoint\n";
    assert!(Ini::parse("settings.conf", source).is_err());
    assert!(Ini::parse("pjsip.conf", source).is_err());
    let result = Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[application]\ntype=user\npassword=first\n[application]\ntype=user\npassword=second\n",
        "",
        "",
        "",
    );
    assert!(matches!(result, Err(ConfigError { file, line: 4, .. }) if file == "ari.conf"));
    assert!(parse("[phone]\ntype=aor\nmax_contacts=1\nmax_contacts=2\n").is_err());
}
