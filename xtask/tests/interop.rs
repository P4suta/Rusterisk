use xtask::reference::{published_address, Interop, ReferenceError};

#[test]
fn callback_inspection_accepts_only_one_unicast_host_address() -> Result<(), ReferenceError> {
    assert_eq!(
        xtask::reference::callback_address(
            "0.250.250.254 STREAM host.docker.internal\n0.250.250.254 DGRAM\n0.250.250.254 RAW\n"
        )?,
        std::net::Ipv4Addr::new(0, 250, 250, 254)
    );
    let plan = Interop::new("fixture", "/workspace/tests/interop/asterisk")?;
    assert_eq!(
        plan.callback_address().arguments,
        [
            "exec",
            "rusterisk-interop-fixture",
            "getent",
            "ahostsv4",
            "host.docker.internal"
        ]
    );
    assert_eq!(xtask::reference::callback_address("198.19.249.254 STREAM host.docker.internal\n198.19.249.254 DGRAM\n198.19.249.254 RAW\n")?, std::net::Ipv4Addr::new(198, 19, 249, 254));
    for invalid in [
        "",
        "0.0.0.0 STREAM",
        "224.0.0.1 STREAM",
        "255.255.255.255 STREAM",
        "192.168.215.1 STREAM\n192.168.215.2 DGRAM",
        "192.168.215.1 UNRECOGNIZED",
        &"x".repeat(513),
    ] {
        assert!(xtask::reference::callback_address(invalid).is_err());
    }
    Ok(())
}

#[test]
fn reference_media_inspection_uses_one_private_container_address() -> Result<(), ReferenceError> {
    let plan = Interop::new("fixture", "/workspace/tests/interop/asterisk")?;
    assert_eq!(
        plan.media_address().arguments,
        [
            "inspect",
            "--format",
            "{{range .NetworkSettings.Networks}}{{.IPAddress}}{{\"\\n\"}}{{end}}",
            "rusterisk-interop-fixture"
        ]
    );
    assert_eq!(
        xtask::reference::media_address("192.168.215.2\n")?,
        std::net::Ipv4Addr::new(192, 168, 215, 2)
    );
    for invalid in [
        "",
        "127.0.0.1",
        "0.0.0.0",
        "8.8.8.8",
        "224.0.0.1",
        "192.168.215.2\n192.168.215.3",
        "::1",
        &"x".repeat(129),
    ] {
        assert!(xtask::reference::media_address(invalid).is_err());
    }
    Ok(())
}

#[test]
fn interop_uses_an_ephemeral_local_port_and_bounded_disposable_state() -> Result<(), ReferenceError>
{
    let plan = Interop::new("fixture", "/workspace/tests/interop/asterisk")?;
    let start = plan.start();
    for argument in [
        "127.0.0.1::8088",
        "--memory=512m",
        "--memory-swap=512m",
        "--cpus=1",
        "--pids-limit=128",
        "--read-only",
        "--cap-drop=ALL",
        "--security-opt=no-new-privileges",
    ] {
        assert!(
            start.arguments.iter().any(|value| value == argument),
            "{argument}"
        );
    }
    assert!(start.arguments.iter().any(|value| value
        == "type=bind,source=/workspace/tests/interop/asterisk,target=/etc/asterisk,readonly"));
    assert_eq!(
        plan.port().arguments,
        ["port", "rusterisk-interop-fixture", "8088/tcp"]
    );
    assert_eq!(
        plan.cleanup().arguments,
        ["rm", "--force", "rusterisk-interop-fixture"]
    );
    assert_eq!(
        published_address("127.0.0.1:49152\n")?,
        "127.0.0.1:49152"
            .parse()
            .map_err(|_| ReferenceError::InvalidAddress)?
    );
    for invalid in [
        "",
        "0.0.0.0:8088",
        "127.0.0.1:0",
        "127.0.0.1:1\n127.0.0.1:2",
        "[::]:8088",
        &"x".repeat(129),
    ] {
        assert!(published_address(invalid).is_err());
    }
    assert!(Interop::new("fixture", "/workspace,other").is_err());
    Ok(())
}
