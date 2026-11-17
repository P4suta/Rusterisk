use rusterisk_api::{
    openapi, swagger_for, DiscoveryFormat, ParameterKind, SwaggerError, OPERATIONS,
};

#[test]
fn music_operations_accept_an_optional_class_and_report_void_control_acceptance(
) -> Result<(), SwaggerError> {
    for (id, nickname, method, parameters) in [
        ("channels.startMoh", "startMoh", "POST", &["mohClass"][..]),
        ("channels.stopMoh", "stopMoh", "DELETE", &[][..]),
    ] {
        let operation = OPERATIONS
            .iter()
            .find(|operation| operation.id == id)
            .ok_or_else(|| SwaggerError::Schema(id.into()))?;
        assert_eq!(operation.nickname, nickname);
        assert_eq!(operation.method.upper(), method);
        assert_eq!(operation.path, "/channels/{channelId}/moh");
        assert_eq!(operation.parameters, parameters);
        assert_eq!(operation.response, "void");
        assert_eq!(operation.success_status, 204);
        assert_eq!(
            operation.parameter_kind("mohClass"),
            if method == "POST" {
                Some(ParameterKind::String)
            } else {
                None
            }
        );
    }
    Ok(())
}

#[test]
fn generated_music_discovery_preserves_methods_optional_class_and_empty_success_responses(
) -> Result<(), SwaggerError> {
    let openapi = openapi();
    let path = "/channels/{channelId}/moh";
    for (method, nickname) in [("post", "startMoh"), ("delete", "stopMoh")] {
        let operation = &openapi["paths"][path][method];
        assert_eq!(operation["operationId"], format!("channels.{nickname}"));
        assert!(operation["responses"]["204"]["content"].is_null());
        assert!(operation["responses"].get("204").is_some());
        if method == "post" {
            let parameter = operation["parameters"]
                .as_array()
                .and_then(|parameters| {
                    parameters
                        .iter()
                        .find(|parameter| parameter["name"] == "mohClass")
                })
                .ok_or_else(|| SwaggerError::Schema("mohClass".into()))?;
            assert_eq!(parameter["in"], "query");
            assert_eq!(parameter["required"], false);
            assert_eq!(parameter["schema"]["type"], "string");
        }
        for format in [DiscoveryFormat::Asterisk, DiscoveryFormat::Standard] {
            let swagger = swagger_for("channels", format)?;
            let operation = swagger["apis"]
                .as_array()
                .and_then(|apis| apis.iter().find(|api| api["path"] == path))
                .and_then(|api| api["operations"].as_array())
                .and_then(|operations| {
                    operations
                        .iter()
                        .find(|operation| operation["nickname"] == nickname)
                })
                .ok_or_else(|| SwaggerError::Schema(nickname.into()))?;
            let (method_field, response_field) = match format {
                DiscoveryFormat::Asterisk => ("httpMethod", "responseClass"),
                DiscoveryFormat::Standard => ("method", "type"),
            };
            assert_eq!(operation[method_field], method.to_ascii_uppercase());
            assert_eq!(operation[response_field], "void");
            if method == "post" {
                let parameter = operation["parameters"]
                    .as_array()
                    .and_then(|parameters| {
                        parameters
                            .iter()
                            .find(|parameter| parameter["name"] == "mohClass")
                    })
                    .ok_or_else(|| SwaggerError::Schema("mohClass".into()))?;
                assert_eq!(parameter["paramType"], "query");
                assert_eq!(parameter["required"], false);
                assert_eq!(parameter["type"], "string");
            }
        }
    }
    Ok(())
}
