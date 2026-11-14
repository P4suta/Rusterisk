use rusterisk_api::{openapi, swagger, ChannelId, Event, EventKind, OPERATIONS};
use rusterisk_api::{resources_for, swagger_for, DiscoveryFormat};

#[test]
fn identifiers_are_bounded_path_segments() {
    for value in ["", "../secret", "a/b", "a?b", "a b"] {
        assert!(ChannelId::try_from(value.to_owned()).is_err());
    }
    assert!(ChannelId::try_from("a".repeat(129)).is_err());
    assert!(ChannelId::try_from("custom-channel_123.45".to_owned()).is_ok());
}

#[test]
fn declared_operations_match_complete_paths_and_their_exact_method() {
    for operation in OPERATIONS {
        let path = operation
            .path
            .split('/')
            .map(|part| if part.starts_with('{') { "item" } else { part })
            .collect::<Vec<_>>()
            .join("/");
        assert!(
            operation.matches(operation.method.upper(), &path),
            "{}",
            operation.id
        );
        assert!(!operation.matches("PATCH", &path), "{}", operation.id);
        assert!(
            !operation.matches(operation.method.upper(), &format!("{path}/extra")),
            "{}",
            operation.id
        );
        assert!(
            !operation.matches(operation.method.upper(), &format!("/extra{path}")),
            "{}",
            operation.id
        );
    }
}

#[test]
fn discovery_formats_preserve_asterisk_lists_and_standard_array_types(
) -> Result<(), rusterisk_api::SwaggerError> {
    for format in [DiscoveryFormat::Asterisk, DiscoveryFormat::Standard] {
        let document = swagger_for("channels", format)?;
        let apis = document["apis"]
            .as_array()
            .ok_or_else(|| rusterisk_api::SwaggerError::Schema("apis".into()))?;
        let operations = apis
            .iter()
            .find(|api| api["path"] == "/channels")
            .and_then(|api| api["operations"].as_array())
            .ok_or_else(|| rusterisk_api::SwaggerError::Schema("channels".into()))?;
        let list = operations
            .iter()
            .find(|operation| operation["nickname"] == "list")
            .ok_or_else(|| rusterisk_api::SwaggerError::Schema("list".into()))?;
        match format {
            DiscoveryFormat::Asterisk => {
                assert_eq!(document["swaggerVersion"], "1.1");
                assert_eq!(list["responseClass"], "List[Channel]");
                assert!(list.get("type").is_none());
                assert_eq!(list["httpMethod"], "GET");
            }
            DiscoveryFormat::Standard => {
                assert_eq!(document["swaggerVersion"], "1.2");
                assert_eq!(list["type"], "array");
                assert_eq!(list["items"]["$ref"], "Channel");
                assert_eq!(list["method"], "GET");
                assert!(list["responseMessages"]
                    .as_array()
                    .is_some_and(|messages| messages
                        .iter()
                        .all(|message| message["message"].is_string())));
            }
        }
        assert_eq!(
            resources_for(format)["swaggerVersion"],
            document["swaggerVersion"]
        );
    }
    Ok(())
}

#[test]
fn event_wire_shape_preserves_asterisk_discriminator() -> Result<(), serde_json::Error> {
    let event: Event = serde_json::from_str(
        r#"{"type":"StasisStart","timestamp":"2026-11-15T00:00:00.000+0900","application":"test","args":[],"channel":{"id":"phone","name":"PJSIP/phone-1","state":"Up","accountcode":"","caller":{"name":"","number":"100"},"connected":{"name":"","number":""},"dialplan":{"context":"internal","exten":"100","priority":1,"app_name":"Stasis","app_data":"test"},"creationtime":"2026-11-15T00:00:00.000+0900","language":"en"}}"#,
    )?;
    assert!(matches!(event.kind, EventKind::StasisStart { .. }));
    let json = serde_json::to_value(event)?;
    assert_eq!(json["type"], "StasisStart");
    assert_eq!(json["channel"]["state"], "Up");
    assert!(json.get("kind").is_none());
    Ok(())
}

#[test]
fn both_descriptions_cover_the_authoritative_operation_set(
) -> Result<(), rusterisk_api::SwaggerError> {
    let openapi = openapi();
    assert_eq!(openapi["openapi"], "3.1.0");
    let mut ids = std::collections::BTreeSet::new();
    for operation in OPERATIONS {
        assert!(ids.insert(operation.id));
        assert_eq!(
            openapi["paths"][operation.path][operation.method.as_str()]["operationId"],
            operation.id
        );
        let resource = operation.resource();
        let definition = swagger(resource)?;
        assert_eq!(definition["swaggerVersion"], "1.2");
        let apis = definition["apis"].as_array();
        assert!(
            apis.is_some_and(|apis| apis.iter().any(|api| api["operations"]
                .as_array()
                .is_some_and(|items| items
                    .iter()
                    .any(|item| item["nickname"] == operation.nickname))))
        );
    }
    Ok(())
}

#[test]
fn openapi_references_resolve_and_body_parameters_use_request_body() {
    fn verify(value: &serde_json::Value, root: &serde_json::Value) {
        match value {
            serde_json::Value::Object(fields) => {
                if let Some(reference) = fields.get("$ref").and_then(serde_json::Value::as_str) {
                    assert!(
                        reference
                            .strip_prefix('#')
                            .is_some_and(|pointer| root.pointer(pointer).is_some()),
                        "unresolved reference {reference}"
                    );
                }
                assert_ne!(
                    fields.get("in").and_then(serde_json::Value::as_str),
                    Some("body")
                );
                for item in fields.values() {
                    verify(item, root);
                }
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    verify(item, root);
                }
            }
            serde_json::Value::Null
            | serde_json::Value::Bool(_)
            | serde_json::Value::Number(_)
            | serde_json::Value::String(_) => {}
        }
    }
    let document = openapi();
    verify(&document, &document);
}

#[test]
fn swagger_event_models_use_wire_discriminators_and_model_identifiers(
) -> Result<(), rusterisk_api::SwaggerError> {
    let document = swagger("events")?;
    let models = &document["models"];
    assert_eq!(
        models["StasisStart"]["id"], "StasisStart",
        "event schema: {}",
        models["Event"]
    );
    assert_eq!(
        models["StasisStart"]["properties"]["channel"]["$ref"],
        "Channel"
    );
    assert_eq!(
        models["StasisStart"]["properties"]["args"]["items"]["type"],
        "string"
    );
    assert_eq!(
        models["ChannelDtmfReceived"]["properties"]["digit"]["type"],
        "string"
    );
    assert_eq!(models["Channel"]["id"], "Channel");
    assert_eq!(models["Channel"]["properties"]["state"]["type"], "string");
    assert!(models["Channel"]["properties"]["state"]["enum"]
        .as_array()
        .is_some_and(|values| values.iter().any(|value| value == "Up")));
    Ok(())
}

#[test]
fn swagger_models_resolve_flat_references_and_keep_inheritance_disjoint(
) -> Result<(), rusterisk_api::SwaggerError> {
    fn property(value: &serde_json::Value, models: &serde_json::Map<String, serde_json::Value>) {
        for unsupported in ["properties", "oneOf", "anyOf", "allOf", "const", "$schema"] {
            assert!(
                value.get(unsupported).is_none(),
                "unsupported property field {unsupported}: {value}"
            );
        }
        if let Some(reference) = value.get("$ref").and_then(serde_json::Value::as_str) {
            assert!(models.contains_key(reference), "unknown model {reference}");
        } else {
            assert!(value
                .get("type")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|kind| matches!(
                    kind,
                    "string" | "integer" | "number" | "boolean" | "array"
                )));
        }
        for bound in ["minimum", "maximum"] {
            if let Some(value) = value.get(bound) {
                assert!(value.is_string());
            }
        }
        if let Some(items) = value.get("items") {
            assert_ne!(items["type"], "array");
            property(items, models);
        }
    }
    let document = swagger("events")?;
    let models = document["models"]
        .as_object()
        .ok_or(rusterisk_api::SwaggerError::Capacity)?;
    for (name, model) in models {
        assert_eq!(model["id"], *name);
        let properties = model["properties"]
            .as_object()
            .ok_or(rusterisk_api::SwaggerError::Capacity)?;
        for value in properties.values() {
            property(value, models);
        }
        if let Some(required) = model["required"].as_array() {
            assert!(required.iter().all(|name| name
                .as_str()
                .is_some_and(|name| properties.contains_key(name))));
        }
        if let Some(subtypes) = model["subTypes"].as_array() {
            assert_eq!(model["discriminator"], "type");
            assert!(model["required"]
                .as_array()
                .is_some_and(|values| values.iter().any(|value| value == "type")));
            for subtype in subtypes {
                let subtype = subtype
                    .as_str()
                    .ok_or(rusterisk_api::SwaggerError::Capacity)?;
                let fields = models[subtype]["properties"]
                    .as_object()
                    .ok_or(rusterisk_api::SwaggerError::Capacity)?;
                assert!(fields.keys().all(|name| !properties.contains_key(name)));
            }
        }
    }
    let source = rusterisk_api::schemas();
    let branches = source["Event"]["oneOf"]
        .as_array()
        .ok_or(rusterisk_api::SwaggerError::Capacity)?;
    for branch in branches {
        let name = branch["properties"]["type"]["const"]
            .as_str()
            .ok_or(rusterisk_api::SwaggerError::Capacity)?;
        assert!(models.contains_key(name));
    }
    Ok(())
}
