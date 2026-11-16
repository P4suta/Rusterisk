use rusterisk_config::{Config, ConfigError};

fn config(algorithms: Option<&str>) -> Result<Config, ConfigError> {
    let algorithms = algorithms
        .map(|value| format!("supported_algorithms_uas={value}\n"))
        .unwrap_or_default();
    Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=sample\n",
        &format!("[account]\ntype=auth\nusername=device\npassword=sample\n{algorithms}"),
        "",
        "",
    )
}

#[test]
fn inbound_digest_algorithms_keep_the_reference_default_and_configured_preference(
) -> Result<(), ConfigError> {
    for (source, expected) in [
        (None, ["MD5"].as_slice()),
        (Some("SHA-256"), ["SHA-256"].as_slice()),
        (
            Some("sha-512-256, md5, SHA-256"),
            ["SHA-512-256", "MD5", "SHA-256"].as_slice(),
        ),
    ] {
        let config = config(source)?;
        let algorithms: Vec<_> = config.auth[0]
            .supported_algorithms_uas
            .iter()
            .map(|algorithm| algorithm.name())
            .collect();
        assert_eq!(algorithms, expected);
        assert_eq!(config.auth[0].username, "device");
        assert_eq!(config.auth[0].inbound_realm(), "asterisk");
    }
    Ok(())
}

#[test]
fn empty_unknown_duplicate_and_excessive_algorithm_lists_identify_the_option_line() {
    for source in [
        "",
        "SHA-384",
        "SHA-256,",
        ",MD5",
        "MD5,,SHA-256",
        "MD5,md5",
        "SHA-256,SHA-256",
        "MD5,SHA-256,SHA-512-256,MD5",
        "MD5-sess",
        "SHA-256 SHA-512-256",
    ] {
        assert!(
            matches!(config(Some(source)), Err(error) if error.file == "pjsip.conf" && error.line == 5 && error.reason == "supported_algorithms_uas requires one to three distinct algorithms: MD5, SHA-256, SHA-512-256"),
            "{source}"
        );
    }
}
