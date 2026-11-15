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
