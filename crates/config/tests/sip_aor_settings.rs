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
fn dynamic_registration_requires_an_explicit_positive_contact_limit() -> Result<(), ConfigError> {
    assert_eq!(parse("")?.aors[0].max_contacts, 0);
    assert_eq!(parse("max_contacts=0\n")?.aors[0].max_contacts, 0);
    assert_eq!(parse("max_contacts=16\n")?.aors[0].max_contacts, 16);
    Ok(())
}

#[test]
fn contact_replacement_defaults_to_refusal_and_accepts_both_flag_values() -> Result<(), ConfigError>
{
    assert!(!parse("")?.aors[0].remove_existing);
    assert!(!parse("remove_existing=no\n")?.aors[0].remove_existing);
    assert!(parse("remove_existing=yes\n")?.aors[0].remove_existing);
    Ok(())
}

#[test]
fn an_invalid_contact_replacement_flag_identifies_the_option_line() {
    let result = parse("remove_existing=sometimes\n");
    assert!(matches!(result, Err(ConfigError { file, line: 3, .. }) if file == "pjsip.conf"));
}

#[test]
fn excessive_contact_limits_identify_the_option_line() {
    for value in ["17", "65536", "-1", "invalid"] {
        let result = parse(&format!("max_contacts={value}\n"));
        assert!(matches!(result, Err(ConfigError { file, line: 3, .. }) if file == "pjsip.conf"));
    }
}
