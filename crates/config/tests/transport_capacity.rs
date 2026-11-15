use rusterisk_config::{Config, ConfigError};

fn parse(count: usize) -> Result<Config, ConfigError> {
    let transports: String = (0..count)
        .map(|index| {
            format!("[transport-{index}]\ntype=transport\nprotocol=udp\nbind=127.0.0.1:0\n")
        })
        .collect();
    Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=test\n",
        &transports,
        "",
        "",
    )
}

#[test]
fn listener_capacity_is_validated_at_the_offending_transport_section() -> Result<(), ConfigError> {
    assert_eq!(parse(16)?.transports.len(), 16);
    assert!(matches!(parse(17), Err(ConfigError { file, line: 65, .. }) if file == "pjsip.conf"));
    Ok(())
}
