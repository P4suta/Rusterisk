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

#[test]
fn session_spelling_is_bounded_and_keeps_the_validated_binary_identity() -> Result<(), String> {
    let address = "127.0.0.1:12000"
        .parse()
        .map_err(|error: std::net::AddrParseError| error.to_string())?;
    for (bytes, text) in [
        ([0; 16], "00000000-0000-0000-0000-000000000000"),
        ([0xff; 16], "ffffffff-ffff-ffff-ffff-ffffffffffff"),
        (
            [
                0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab,
                0xcd, 0xef,
            ],
            "01234567-89ab-cdef-0123-456789abcdef",
        ),
    ] {
        let target = AudioSocketTarget::new(address, bytes).map_err(|error| error.to_string())?;
        assert_eq!(target.session_text(), text);
    }
    let bytes = [
        0xab, 0xcd, 0xef, 0xab, 0xcd, 0xef, 0x4a, 0xbc, 0x8d, 0xef, 0xab, 0xcd, 0xef, 0xab, 0xcd,
        0xef,
    ];
    let target = AudioSocketTarget::new(address, bytes).map_err(|error| error.to_string())?;
    assert_eq!(
        target.session_text(),
        "abcdefab-cdef-4abc-8def-abcdefabcdef"
    );
    for text in [
        "ABCDEFAB-CDEF-4ABC-8DEF-ABCDEFABCDEF",
        "aBcDeFaB-cDeF-4aBc-8DeF-AbCdEfAbCdEf",
        "abcdefab-cdef-4abc-8def-abcdefabcdef",
    ] {
        let target = target
            .with_session_text(text)
            .map_err(|error| error.to_string())?;
        assert_eq!(target.remote(), address);
        assert_eq!(target.session(), bytes);
        assert_eq!(target.session_text(), text);
    }
    Ok(())
}

#[test]
fn session_spelling_rejects_different_bytes_and_nonhyphenated_uuid_forms() -> Result<(), String> {
    let target = AudioSocketTarget::new(
        "127.0.0.1:12000"
            .parse()
            .map_err(|error: std::net::AddrParseError| error.to_string())?,
        [0xab; 16],
    )
    .map_err(|error| error.to_string())?;
    for text in [
        "",
        "abababab-abab-abab-abab-abababababa",
        "abababab-abab-abab-abab-ababababababa",
        "abababababababababababababababab",
        "{abababab-abab-abab-abab-abababababab}",
        "urn:uuid:abababab-abab-abab-abab-abababababab",
        "abababab_abab-abab-abab-abababababab",
        "abababab-abab-abab-abab-abababababag",
        "abababab-abab-abab-abab-abababababac",
        "abababab-abab-abab-abab-abababababaé",
    ] {
        assert!(target.with_session_text(text).is_err(), "{text:?}");
    }
    Ok(())
}
