use rusterisk_api::EventSource;

#[test]
fn source_parameters_describe_bounded_comma_separated_lists(
) -> Result<(), rusterisk_api::SwaggerError> {
    let description = rusterisk_api::openapi();
    let parameter = &description["paths"]["/applications/{applicationName}/subscription"]["post"]
        ["parameters"][1];
    assert_eq!(parameter["schema"]["type"], "array");
    assert_eq!(parameter["schema"]["maxItems"], 32);
    assert_eq!(parameter["explode"], false);
    for format in [
        rusterisk_api::DiscoveryFormat::Asterisk,
        rusterisk_api::DiscoveryFormat::Standard,
    ] {
        let description = rusterisk_api::swagger_for("applications", format)?;
        let operation = description["apis"]
            .as_array()
            .and_then(|apis| {
                apis.iter()
                    .find(|api| api["path"] == "/applications/{applicationName}/subscription")
            })
            .ok_or_else(|| rusterisk_api::SwaggerError::Schema("subscription".into()))?;
        let parameter = &operation["operations"][0]["parameters"][1];
        assert_eq!(parameter["allowMultiple"], true);
    }
    Ok(())
}

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
