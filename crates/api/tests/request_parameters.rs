use rusterisk_api::{openapi, ParameterKind, OPERATIONS};

#[test]
fn operation_parameters_describe_the_same_value_kinds_for_requests_and_discovery() {
    for (operation, name, expected) in [
        ("channels.originate", "endpoint", ParameterKind::String),
        ("channels.originate", "timeout", ParameterKind::Integer),
        ("channels.continue", "priority", ParameterKind::Integer),
        ("channels.sendDTMF", "duration", ParameterKind::Integer),
        ("channels.record", "beep", ParameterKind::Boolean),
        (
            "events.eventWebsocket",
            "subscribeAll",
            ParameterKind::Boolean,
        ),
        ("bridges.addChannel", "channel", ParameterKind::StringList),
        (
            "applications.subscribe",
            "eventSource",
            ParameterKind::StringList,
        ),
        (
            "channels.externalMedia",
            "variables",
            ParameterKind::Variables,
        ),
        ("applications.filter", "filter", ParameterKind::Filter),
    ] {
        let operation = OPERATIONS.iter().find(|entry| entry.id == operation);
        assert!(operation.is_some());
        if let Some(operation) = operation {
            assert_eq!(operation.parameter_kind(name), Some(expected));
            assert_eq!(operation.parameter_kind("unknown"), None);
        }
    }
}

#[test]
fn json_request_schemas_include_typed_query_fields_and_keep_path_identifiers_out() {
    let document = openapi();
    for (path, method, name, kind) in [
        ("/channels", "post", "endpoint", "string"),
        ("/channels", "post", "timeout", "integer"),
        (
            "/channels/{channelId}/continue",
            "post",
            "priority",
            "integer",
        ),
        ("/bridges/{bridgeId}/addChannel", "post", "channel", "array"),
        ("/channels/{channelId}/record", "post", "beep", "boolean"),
    ] {
        let operation = &document["paths"][path][method];
        assert_eq!(
            operation["requestBody"]["content"]["application/json"]["schema"]["properties"][name]
                ["type"],
            kind,
            "{path} {name}"
        );
        let parameters = operation["parameters"].as_array();
        assert!(parameters.is_some());
        if let Some(parameters) = parameters {
            assert!(parameters
                .iter()
                .any(|parameter| parameter["name"] == name && parameter["schema"]["type"] == kind));
        }
        for identifier in ["channelId", "bridgeId"] {
            if path.contains(&format!("{{{identifier}}}")) {
                assert!(
                    operation["requestBody"]["content"]["application/json"]["schema"]["properties"]
                        [identifier]
                        .is_null()
                );
            }
        }
    }
    assert_eq!(
        document["paths"]["/channels"]["post"]["requestBody"]["content"]["application/json"]
            ["schema"]["properties"]["variables"]["additionalProperties"]["type"],
        "string"
    );
}
