use std::{net::SocketAddr, process::Command};
use xtask::distribution::SdkHttpTarget;

fn address(value: &str) -> Result<SocketAddr, std::io::Error> {
    value.parse().map_err(std::io::Error::other)
}

#[test]
fn only_loopback_addresses_with_a_bound_port_are_admitted() -> Result<(), std::io::Error> {
    for value in ["127.0.0.1:8088", "127.1.2.3:1", "[::1]:65535"] {
        let address = address(value)?;
        let target = SdkHttpTarget::new(address, "api", "fixture", "0.1.0", "sdk-run")
            .map_err(std::io::Error::other)?;
        assert_eq!(target.address(), address);
    }
    for value in ["127.0.0.1:0", "0.0.0.0:8088", "10.0.0.1:8088", "[::]:8088"] {
        assert!(SdkHttpTarget::new(address(value)?, "api", "fixture", "0.1.0", "sdk-run").is_err());
    }
    Ok(())
}

#[test]
fn fixture_credentials_are_bounded_and_cannot_inject_headers_or_environment_entries(
) -> Result<(), std::io::Error> {
    let address = address("127.0.0.1:8088")?;
    for (username, password) in [
        ("", "fixture"),
        ("api:other", "fixture"),
        ("api\n", "fixture"),
        ("api", ""),
        ("api", "fixture\r\n"),
        ("api", "fixture\0"),
    ] {
        assert!(SdkHttpTarget::new(address, username, password, "0.1.0", "sdk-run").is_err());
    }
    let username = "u".repeat(128);
    let password = "p".repeat(1024);
    SdkHttpTarget::new(address, &username, &password, "0.1.0", "sdk-run")
        .map_err(std::io::Error::other)?;
    assert!(SdkHttpTarget::new(address, &(username + "u"), "fixture", "0.1.0", "sdk-run").is_err());
    assert!(SdkHttpTarget::new(address, "api", &(password + "p"), "0.1.0", "sdk-run").is_err());
    Ok(())
}

#[test]
fn expected_version_and_the_reserved_bridge_id_have_explicit_contracts(
) -> Result<(), std::io::Error> {
    let address = address("127.0.0.1:8088")?;
    for version in ["", "latest", "0.1", "0.1.0\n"] {
        assert!(SdkHttpTarget::new(address, "api", "fixture", version, "sdk-run").is_err());
    }
    for identifier in ["", ".", "..", "sdk/run", "sdk?run", "sdk run", "sdk\0run"] {
        assert!(SdkHttpTarget::new(address, "api", "fixture", "0.1.0", identifier).is_err());
    }
    let identifier = "i".repeat(128);
    SdkHttpTarget::new(address, "api", "fixture", "0.1.0-beta.1+local", &identifier)
        .map_err(std::io::Error::other)?;
    assert!(SdkHttpTarget::new(address, "api", "fixture", "0.1.0", &(identifier + "i")).is_err());
    Ok(())
}

#[test]
fn child_configuration_uses_environment_values_without_secret_arguments(
) -> Result<(), std::io::Error> {
    let target = SdkHttpTarget::new(
        address("[::1]:8088")?,
        "fixture-user",
        "fixture-password",
        "0.1.0",
        "sdk-run",
    )
    .map_err(std::io::Error::other)?;
    let mut command = Command::new("consumer");
    target.configure(&mut command);
    assert!(command.get_args().next().is_none());
    let values: std::collections::BTreeMap<_, _> = command.get_envs().collect();
    for (key, expected) in [
        ("RUSTERISK_SDK_HTTP_ADDRESS", "http://[::1]:8088"),
        ("RUSTERISK_SDK_HTTP_USERNAME", "fixture-user"),
        ("RUSTERISK_SDK_HTTP_PASSWORD", "fixture-password"),
        ("RUSTERISK_SDK_HTTP_VERSION", "0.1.0"),
        ("RUSTERISK_SDK_HTTP_BRIDGE", "sdk-run"),
    ] {
        assert_eq!(
            values.get(std::ffi::OsStr::new(key)).copied().flatten(),
            Some(std::ffi::OsStr::new(expected))
        );
    }
    assert_eq!(values.len(), 5);
    Ok(())
}
