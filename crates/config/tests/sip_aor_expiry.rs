use rusterisk_config::{Config, ConfigError};

fn parse(options: &str) -> Result<Config, ConfigError> {
    Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[application]\ntype=user\npassword=sample\n",
        &format!("[phone]\ntype=aor\n{options}"),
        "",
        "",
    )
}

#[test]
fn address_expiry_defaults_match_the_reference_seconds() -> Result<(), ConfigError> {
    let config = parse("")?;
    let address = &config.aors[0];
    assert_eq!(address.minimum_expiration, 60);
    assert_eq!(address.default_expiration, 3600);
    assert_eq!(address.maximum_expiration, 7200);
    Ok(())
}

#[test]
fn expiry_settings_preserve_unsigned_seconds_and_leave_default_clamping_to_registration(
) -> Result<(), ConfigError> {
    let config =
        parse("minimum_expiration=0\ndefault_expiration=4294967295\nmaximum_expiration=86400\n")?;
    assert_eq!(config.aors[0].minimum_expiration, 0);
    assert_eq!(config.aors[0].default_expiration, u32::MAX);
    assert_eq!(config.aors[0].maximum_expiration, 86400);
    let config = parse("minimum_expiration=7200\nmaximum_expiration=7200\n")?;
    assert_eq!(config.aors[0].default_expiration, 3600);
    Ok(())
}

#[test]
fn invalid_or_inverted_expiry_bounds_identify_the_setting_line() {
    for (options, line) in [
        ("minimum_expiration=7201\n", 3),
        ("minimum_expiration=61\nmaximum_expiration=60\n", 3),
        ("minimum_expiration=-1\n", 3),
        ("default_expiration=4294967296\n", 3),
        ("maximum_expiration=invalid\n", 3),
    ] {
        let result = parse(options);
        assert!(
            matches!(result, Err(ConfigError { file, line: actual, .. }) if file == "pjsip.conf" && actual == line)
        );
    }
}
