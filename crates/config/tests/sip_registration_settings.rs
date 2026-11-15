use rusterisk_config::{Config, ConfigError};

const BASE: &str = "[udp]\ntype=transport\nprotocol=udp\nbind=127.0.0.1:5060\n[credential]\ntype=auth\nusername=account\npassword=sample\n";

fn registration(name: &str, settings: &str) -> String {
    format!("[{name}]\ntype=registration\ntransport=udp\noutbound_auth=credential\nserver_uri=sip:provider.example\nclient_uri=sip:account@provider.example\n{settings}")
}

fn config(source: &str) -> Result<Config, ConfigError> {
    Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=sample\n",
        source,
        "",
        "",
    )
}

#[test]
fn registration_defaults_match_the_reference_refresh_and_retry_settings() -> Result<(), ConfigError>
{
    let config = config(&format!("{BASE}{}", registration("provider", "")))?;
    let registration = &config.registrations[0];
    assert_eq!(registration.expiration, 3600);
    assert_eq!(registration.contact_user, "s");
    assert_eq!(registration.retry_interval, 60);
    assert_eq!(registration.forbidden_retry_interval, 0);
    assert_eq!(registration.fatal_retry_interval, 0);
    assert_eq!(registration.max_retries, 10);
    assert_eq!(registration.max_random_initial_delay, 10);
    assert!(registration.auth_rejection_permanent);
    Ok(())
}

#[test]
fn registration_timers_preserve_unsigned_seconds_and_explicit_retry_policy(
) -> Result<(), ConfigError> {
    let source = format!("{BASE}{}", registration("provider", "expiration=4294967295\ncontact_user=trunk+1\nretry_interval=1\nforbidden_retry_interval=2\nfatal_retry_interval=3\nmax_retries=0\nmax_random_initial_delay=0\nauth_rejection_permanent=no\n"));
    let config = config(&source)?;
    let registration = &config.registrations[0];
    assert_eq!(registration.expiration, u32::MAX);
    assert_eq!(registration.contact_user, "trunk+1");
    assert_eq!(registration.retry_interval, 1);
    assert_eq!(registration.forbidden_retry_interval, 2);
    assert_eq!(registration.fatal_retry_interval, 3);
    assert_eq!(registration.max_retries, 0);
    assert_eq!(registration.max_random_initial_delay, 0);
    assert!(!registration.auth_rejection_permanent);
    Ok(())
}

#[test]
fn malformed_registration_settings_identify_the_option_line() {
    for setting in [
        "expiration=-1",
        "expiration=4294967296",
        "retry_interval=1.5",
        "forbidden_retry_interval=-1",
        "fatal_retry_interval=invalid",
        "max_retries=-1",
        "max_random_initial_delay=-1",
        "auth_rejection_permanent=maybe",
        "contact_user=",
        "contact_user=unsafe@host",
        "contact_user=unsafe value",
        "contact_user=unsafe%zz",
    ] {
        let source = format!(
            "{BASE}{}",
            registration("provider", &format!("{setting}\n"))
        );
        let line = source
            .lines()
            .position(|line| line == setting)
            .map(|line| line + 1);
        let error = config(&source).err();
        assert!(
            matches!(error, Some(error) if error.file == "pjsip.conf" && Some(error.line) == line),
            "{setting}"
        );
    }
}

#[test]
fn registration_client_capacity_is_rejected_at_the_overflowing_section() -> Result<(), ConfigError>
{
    let mut source = BASE.to_owned();
    for index in 0..16 {
        source.push_str(&registration(&format!("provider{index}"), ""));
    }
    assert_eq!(config(&source)?.registrations.len(), 16);
    let line = source.lines().count() + 1;
    source.push_str(&registration("overflow", ""));
    assert!(
        matches!(config(&source), Err(error) if error.file == "pjsip.conf" && error.line == line && error.reason.contains("16"))
    );
    Ok(())
}
