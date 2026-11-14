use rusterisk_api::{openapi, swagger, ChannelId, Event, EventKind, OPERATIONS};

#[test]
fn identifiers_are_bounded_path_segments() {
    for value in ["", "../secret", "a/b", "a?b", "a b"] {
        assert!(ChannelId::try_from(value.to_owned()).is_err());
    }
    assert!(ChannelId::try_from("a".repeat(129)).is_err());
    assert!(ChannelId::try_from("custom-channel_123.45".to_owned()).is_ok());
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
fn both_descriptions_cover_the_authoritative_operation_set() {
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
        let definition = swagger(resource);
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
