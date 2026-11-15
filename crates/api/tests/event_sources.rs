use rusterisk_api::EventSource;

#[test]
fn event_source_uris_keep_kind_identifiers_and_wildcard_scope(
) -> Result<(), rusterisk_api::InvalidEventSource> {
    for text in [
        "channel:call",
        "bridge:mix",
        "endpoint:PJSIP/phone",
        "endpoint:PJSIP",
        "channel:",
        "bridge:",
        "endpoint:",
    ] {
        let source = EventSource::try_from(text)?;
        assert_eq!(source.to_string(), text);
    }
    for invalid in [
        "channel",
        "other:call",
        "channel:../call",
        "bridge:mix,channel:call",
        "endpoint:PJSIP/",
        "endpoint:/phone",
        "endpoint:PJSIP/phone/extra",
    ] {
        assert!(EventSource::try_from(invalid).is_err(), "{invalid}");
    }
    assert!(EventSource::try_from(format!("channel:{}", "a".repeat(129)).as_str()).is_err());
    Ok(())
}
