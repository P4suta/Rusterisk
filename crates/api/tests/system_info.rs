use rusterisk_api::{openapi, AsteriskInfo};

#[test]
fn system_information_preserves_every_optional_section_and_configuration_limit(
) -> Result<(), serde_json::Error> {
    let sections = serde_json::json!({
        "build": {"os":"Linux", "kernel":"6.12", "options":"", "machine":"aarch64", "date":"2026-11-16", "user":"builder"},
        "system": {"version":"22.9.0", "entity_id":"00:11:22:33:44:55"},
        "config": {"name":"reference", "default_language":"en", "max_channels":64, "max_open_files":4096, "max_load":1.25, "setid":{"user":"65532", "group":"65532"}},
        "status": {"startup_time":"2026-11-15T17:00:00Z", "last_reload_time":"2026-11-15T17:01:00Z"}
    });
    for mask in 0..16 {
        let mut value = serde_json::json!({});
        for (index, name) in ["build", "system", "config", "status"]
            .into_iter()
            .enumerate()
        {
            if mask & (1 << index) != 0 {
                value[name] = sections[name].clone();
            }
        }
        let info: AsteriskInfo = serde_json::from_value(value.clone())?;
        assert_eq!(serde_json::to_value(info)?, value);
    }
    let minimal = serde_json::json!({"config":{"name":"reference", "default_language":"en", "setid":{"user":"", "group":""}}});
    let info: AsteriskInfo = serde_json::from_value(minimal.clone())?;
    assert_eq!(serde_json::to_value(info)?, minimal);
    Ok(())
}

#[test]
fn information_descriptions_keep_all_sections_optional() -> Result<(), rusterisk_api::SwaggerError>
{
    let document = openapi();
    let schema = &document["components"]["schemas"]["AsteriskInfo"];
    let legacy = rusterisk_api::swagger("asterisk")?;
    for model in [schema, &legacy["models"]["AsteriskInfo"]] {
        assert!(model
            .get("required")
            .is_none_or(|required| required.as_array().is_some_and(Vec::is_empty)));
        for name in ["build", "system", "config", "status"] {
            assert!(model["properties"].get(name).is_some());
        }
    }
    assert_eq!(
        legacy["models"]["ConfigInfo"]["properties"]["max_load"]["type"],
        "number"
    );
    assert_eq!(
        legacy["models"]["ConfigInfo"]["properties"]["max_load"]["format"],
        "double"
    );
    Ok(())
}

#[test]
fn information_selection_describes_comma_separated_known_sections(
) -> Result<(), rusterisk_api::SwaggerError> {
    let expected = serde_json::Value::Array(
        ["build", "system", "config", "status"]
            .into_iter()
            .map(|value| serde_json::Value::String(value.into()))
            .collect(),
    );
    let document = openapi();
    let parameters = document["paths"]["/asterisk/info"]["get"]["parameters"].as_array();
    assert!(parameters.is_some());
    let parameter = parameters.and_then(|parameters| {
        parameters
            .iter()
            .find(|parameter| parameter["name"] == "only")
    });
    assert!(parameter.is_some());
    if let Some(parameter) = parameter {
        assert_eq!(parameter["schema"]["type"], "array");
        assert_eq!(parameter["schema"]["items"]["enum"], expected);
        assert_eq!(parameter["explode"], false);
    }
    let legacy = rusterisk_api::swagger_for("asterisk", rusterisk_api::DiscoveryFormat::Asterisk)?;
    let selected = legacy["apis"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|entry| entry["path"] == "/asterisk/info");
    assert!(selected.is_some());
    if let Some(selected) = selected {
        let parameter = selected["operations"][0]["parameters"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|parameter| parameter["name"] == "only");
        assert!(parameter.is_some());
        if let Some(parameter) = parameter {
            assert_eq!(parameter["dataType"], "string");
            assert_eq!(parameter["allowMultiple"], true);
            assert_eq!(parameter["allowableValues"]["values"], expected);
        }
    }
    Ok(())
}
