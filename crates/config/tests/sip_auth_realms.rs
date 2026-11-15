use rusterisk_config::{Config, ConfigError};

fn config(realm: Option<&str>) -> Result<Config, ConfigError> {
    let realm = realm
        .map(|value| format!("realm={value}\n"))
        .unwrap_or_default();
    Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[application]\ntype=user\npassword=sample\n",
        &format!("[credential]\ntype=auth\nusername=device\npassword=sample\n{realm}"),
        "",
        "",
    )
}

#[test]
fn unspecified_realms_keep_inbound_and_outbound_roles_distinct() -> Result<(), ConfigError> {
    let config = config(None)?;
    let authentication = &config.auth[0];
    assert!(authentication.realm.is_empty());
    assert_eq!(authentication.inbound_realm(), "asterisk");
    assert!(authentication.matches_outbound_realm("provider.example"));
    assert!(authentication.matches_outbound_realm("another-provider"));
    Ok(())
}

#[test]
fn named_realms_match_exactly_and_an_outbound_wildcard_remains_explicit() -> Result<(), ConfigError>
{
    let named = config(Some("provider.example"))?;
    let named = &named.auth[0];
    assert_eq!(named.inbound_realm(), "provider.example");
    assert!(named.matches_outbound_realm("provider.example"));
    assert!(!named.matches_outbound_realm("PROVIDER.example"));
    assert!(!named.matches_outbound_realm("another-provider"));
    let wildcard = config(Some("*"))?;
    let wildcard = &wildcard.auth[0];
    assert_eq!(wildcard.inbound_realm(), "*");
    assert!(wildcard.matches_outbound_realm("provider.example"));
    Ok(())
}
