use rusterisk_api::AudioSocketTarget;

#[test]
fn stream_targets_preserve_session_bytes_and_validate_unicast_ports(
) -> Result<(), std::net::AddrParseError> {
    for text in ["127.0.0.1:12000", "127.0.0.1:65535", "[::1]:12000"] {
        let address = text.parse()?;
        let target = AudioSocketTarget::new(address, [7; 16]);
        assert!(
            target.is_ok_and(|target| target.remote() == address && target.session() == [7; 16])
        );
    }
    for text in [
        "127.0.0.1:0",
        "0.0.0.0:12000",
        "[::]:12000",
        "224.0.0.1:12000",
        "[ff02::1]:12000",
        "255.255.255.255:12000",
    ] {
        assert!(AudioSocketTarget::new(text.parse()?, [0; 16]).is_err());
    }
    Ok(())
}
