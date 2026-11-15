use rusterisk_api::{openapi, swagger_for, DiscoveryFormat, RecordingIfExists};

#[test]
fn recording_conflict_policies_roundtrip_their_canonical_wire_values(
) -> Result<(), serde_json::Error> {
    for (policy, text) in [
        (RecordingIfExists::Fail, "fail"),
        (RecordingIfExists::Overwrite, "overwrite"),
        (RecordingIfExists::Append, "append"),
    ] {
        assert_eq!(serde_json::to_value(policy)?, text);
        assert_eq!(
            serde_json::from_value::<RecordingIfExists>(text.into())?,
            policy
        );
        assert_eq!(RecordingIfExists::try_from(text), Ok(policy));
    }
    assert_eq!(RecordingIfExists::default(), RecordingIfExists::Fail);
    assert_eq!(RecordingIfExists::try_from(""), Ok(RecordingIfExists::Fail));
    for (text, policy) in [
        ("FaIl", RecordingIfExists::Fail),
        ("OVERWRITE", RecordingIfExists::Overwrite),
        ("Append", RecordingIfExists::Append),
    ] {
        assert_eq!(RecordingIfExists::try_from(text), Ok(policy));
    }
    for invalid in ["replace", "fail,append", "ｆａｉｌ"] {
        assert!(RecordingIfExists::try_from(invalid).is_err());
    }
    assert!(RecordingIfExists::try_from("overwrite".repeat(4096).as_str()).is_err());
    Ok(())
}

#[test]
fn recording_parameters_derive_policy_values_from_the_public_type(
) -> Result<(), rusterisk_api::SwaggerError> {
    let openapi = openapi();
    let expected = serde_json::Value::Array(
        ["fail", "overwrite", "append"]
            .into_iter()
            .map(|value| serde_json::Value::String(value.into()))
            .collect(),
    );
    for resource in ["channels", "bridges"] {
        let path = format!(
            "/{resource}/{{{}}}/record",
            if resource == "channels" {
                "channelId"
            } else {
                "bridgeId"
            }
        );
        let parameters = openapi["paths"][&path]["post"]["parameters"]
            .as_array()
            .ok_or_else(|| rusterisk_api::SwaggerError::Schema("parameters".into()))?;
        let policy = parameters
            .iter()
            .find(|parameter| parameter["name"] == "ifExists")
            .ok_or_else(|| rusterisk_api::SwaggerError::Schema("ifExists".into()))?;
        assert_eq!(policy["schema"]["enum"], expected);
        let swagger = swagger_for(resource, DiscoveryFormat::Asterisk)?;
        let apis = swagger["apis"]
            .as_array()
            .ok_or_else(|| rusterisk_api::SwaggerError::Schema("apis".into()))?;
        let operation = apis
            .iter()
            .find(|api| api["path"] == path)
            .ok_or_else(|| rusterisk_api::SwaggerError::Schema(path.clone()))?;
        let parameters = operation["operations"][0]["parameters"]
            .as_array()
            .ok_or_else(|| rusterisk_api::SwaggerError::Schema("parameters".into()))?;
        let policy = parameters
            .iter()
            .find(|parameter| parameter["name"] == "ifExists")
            .ok_or_else(|| rusterisk_api::SwaggerError::Schema("ifExists".into()))?;
        assert_eq!(policy["allowableValues"]["values"], expected);
        let standard = swagger_for(resource, DiscoveryFormat::Standard)?;
        let apis = standard["apis"]
            .as_array()
            .ok_or_else(|| rusterisk_api::SwaggerError::Schema("apis".into()))?;
        let operation = apis
            .iter()
            .find(|api| api["path"] == path)
            .ok_or_else(|| rusterisk_api::SwaggerError::Schema(path.clone()))?;
        let parameters = operation["operations"][0]["parameters"]
            .as_array()
            .ok_or_else(|| rusterisk_api::SwaggerError::Schema("parameters".into()))?;
        let policy = parameters
            .iter()
            .find(|parameter| parameter["name"] == "ifExists")
            .ok_or_else(|| rusterisk_api::SwaggerError::Schema("ifExists".into()))?;
        assert_eq!(policy["enum"], expected);
    }
    Ok(())
}
