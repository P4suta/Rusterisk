#[test]
fn omitted_event_filters_differ_from_explicit_empty_lists_and_invalid_nulls(
) -> Result<(), serde_json::Error> {
    let omitted: rusterisk_api::EventFilterRequest = serde_json::from_str("{}")?;
    assert_eq!(omitted, rusterisk_api::EventFilterRequest::default());
    let empty: rusterisk_api::EventFilterRequest = serde_json::from_str(r#"{"allowed":[]}"#)?;
    assert_eq!(empty.allowed, Some(Vec::new()));
    assert_eq!(empty.disallowed, None);
    for invalid in [
        r#"{"allowed":null}"#,
        r#"{"disallowed":{}}"#,
        r#"{"allowed":[{}]}"#,
        r#"{"allowed":[{"type":0}]}"#,
    ] {
        assert!(serde_json::from_str::<rusterisk_api::EventFilterRequest>(invalid).is_err());
    }
    Ok(())
}

#[test]
fn event_filter_discovery_describes_a_partial_json_update(
) -> Result<(), rusterisk_api::SwaggerError> {
    let description = rusterisk_api::openapi();
    let operation = &description["paths"]["/applications/{applicationName}/eventFilter"]["put"];
    assert_eq!(operation["operationId"], "applications.filter");
    let body = &operation["requestBody"]["content"]["application/json"]["schema"];
    assert_eq!(body["$ref"], "#/components/schemas/EventFilterRequest");
    for format in [
        rusterisk_api::DiscoveryFormat::Asterisk,
        rusterisk_api::DiscoveryFormat::Standard,
    ] {
        let description = rusterisk_api::swagger_for("applications", format)?;
        let operation = description["apis"]
            .as_array()
            .and_then(|apis| {
                apis.iter()
                    .find(|api| api["path"] == "/applications/{applicationName}/eventFilter")
            })
            .ok_or_else(|| rusterisk_api::SwaggerError::Schema("eventFilter".into()))?;
        assert_eq!(
            operation["operations"][0]["parameters"][1]["paramType"],
            "body"
        );
        assert_eq!(
            operation["operations"][0]["parameters"][1]["dataType"],
            "object"
        );
    }
    Ok(())
}
