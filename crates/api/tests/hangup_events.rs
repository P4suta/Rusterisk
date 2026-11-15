use rusterisk_api::{openapi, Event, EventKind};

#[test]
fn hangup_requests_preserve_omitted_flags_and_optional_technology_causes(
) -> Result<(), serde_json::Error> {
    let channel: serde_json::Value = serde_json::from_str(
        r#"{"id":"external","name":"WebSocket/INCOMING/peer","state":"Up","accountcode":"","caller":{"name":"","number":""},"connected":{"name":"","number":""},"dialplan":{"context":"default","exten":"s","priority":1,"app_name":"Stasis","app_data":"calls"},"creationtime":"now","language":"en"}"#,
    )?;
    for (cause, soft, technology) in [
        (Some(16), None, None),
        (None, Some(false), None),
        (None, None, None),
        (Some(21), Some(true), Some(-42)),
    ] {
        let mut value = serde_json::json!({"type":"ChannelHangupRequest", "application":"calls", "timestamp":"now", "channel":channel});
        if let Some(cause) = cause {
            value["cause"] = cause.into();
        }
        if let Some(soft) = soft {
            value["soft"] = soft.into();
        }
        if let Some(technology) = technology {
            value["tech_cause"] = technology.into();
        }
        let event: Event = serde_json::from_value(value.clone())?;
        assert!(matches!(event.kind, EventKind::ChannelHangupRequest { .. }));
        assert_eq!(serde_json::to_value(event)?, value);
    }
    Ok(())
}

#[test]
fn hangup_descriptions_keep_optional_fields_and_signed_technology_causes(
) -> Result<(), rusterisk_api::SwaggerError> {
    let document = openapi();
    let branches = document["components"]["schemas"]["Event"]["oneOf"]
        .as_array()
        .ok_or(rusterisk_api::SwaggerError::Capacity)?;
    let schema = branches
        .iter()
        .find(|branch| branch["properties"]["type"]["const"] == "ChannelHangupRequest")
        .ok_or(rusterisk_api::SwaggerError::Capacity)?;
    let legacy = rusterisk_api::swagger("events")?;
    let legacy = &legacy["models"]["ChannelHangupRequest"];
    assert_eq!(legacy["properties"]["tech_cause"]["type"], "integer");
    assert_eq!(legacy["properties"]["tech_cause"]["format"], "int32");
    for schema in [schema, legacy] {
        let required = schema["required"]
            .as_array()
            .ok_or(rusterisk_api::SwaggerError::Capacity)?;
        for name in ["cause", "soft", "tech_cause"] {
            assert!(!required.iter().any(|field| field == name));
        }
    }
    Ok(())
}
