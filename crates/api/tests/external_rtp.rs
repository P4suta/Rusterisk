use rusterisk_api::{ExternalRtpTarget, StaticRtpCodec};

#[test]
fn external_media_discovery_keeps_optional_hosts_and_both_data_fields() {
    let operation = rusterisk_api::OPERATIONS
        .iter()
        .find(|operation| operation.id == "channels.externalMedia");
    assert!(
        operation.is_some_and(|operation| ["external_host", "data", "transport_data"]
            .iter()
            .all(|parameter| operation.parameters.contains(parameter)))
    );
    assert!(!operation.is_some_and(|operation| operation.parameters.contains(&"external_host!")));
}

#[test]
fn external_rtp_targets_keep_the_selected_static_codec_and_unicast_destination(
) -> Result<(), serde_json::Error> {
    for (name, codec, payload) in [
        ("ulaw", StaticRtpCodec::Pcmu, 0),
        ("alaw", StaticRtpCodec::Pcma, 8),
    ] {
        assert_eq!(serde_json::to_value(codec)?, name);
        assert_eq!(codec.payload_type(), payload);
        for text in ["127.0.0.1:12000", "[::1]:12000"] {
            let remote = text.parse().map_err(serde::de::Error::custom)?;
            let target = ExternalRtpTarget::new(remote, codec).map_err(serde::de::Error::custom)?;
            assert_eq!(target.remote(), remote);
            assert_eq!(target.codec(), codec);
        }
    }
    for text in [
        "127.0.0.1:0",
        "127.0.0.1:65535",
        "0.0.0.0:12000",
        "[::]:12000",
        "224.0.0.1:12000",
        "[ff02::1]:12000",
        "255.255.255.255:12000",
    ] {
        let remote = text.parse().map_err(serde::de::Error::custom)?;
        assert!(ExternalRtpTarget::new(remote, StaticRtpCodec::Pcmu).is_err());
    }
    assert!(serde_json::from_str::<StaticRtpCodec>("\"opus\"").is_err());
    Ok(())
}
