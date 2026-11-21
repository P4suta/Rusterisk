use rusterisk_api::{
    openapi, resources_for, swagger_for, DiscoveryFormat, ManagementOperation, Method,
    MANAGEMENT_OPERATIONS, OPERATIONS,
};

#[test]
fn configuration_reload_has_a_separate_closed_management_contract() {
    assert_eq!(
        MANAGEMENT_OPERATIONS,
        &[
            ManagementOperation::ReloadConfiguration,
            ManagementOperation::ChannelRtpObservation,
            ManagementOperation::DiagnoseExtension,
            ManagementOperation::DiagnoseChannel,
            ManagementOperation::TestCall,
        ]
    );
    let operation = ManagementOperation::ReloadConfiguration;
    let metadata = operation.metadata();
    assert_eq!(metadata.id, "configuration.reload");
    assert_eq!(metadata.method, Method::Post);
    assert_eq!(metadata.path, "/rusterisk/configuration/reload");
    assert_eq!(metadata.response, "void");
    assert_eq!(metadata.success_status, 204);
    assert!(metadata.parameters.is_empty());
    assert!(metadata.matches("POST", "/rusterisk/configuration/reload"));
    assert!(!metadata.matches("GET", "/rusterisk/configuration/reload"));
    assert!(!metadata.matches("POST", "/ari/rusterisk/configuration/reload"));
    assert!(OPERATIONS
        .iter()
        .all(|entry| entry.id != metadata.id && entry.path != metadata.path));
    assert!(!rusterisk_api::is_known_operation("POST", metadata.path));
}

#[test]
fn test_call_contract_carries_the_nominated_endpoint_and_deadline() {
    let operation = ManagementOperation::TestCall.metadata();
    assert_eq!(operation.path, rusterisk_api::test_call::PATH);
    let entry = &openapi()["paths"][operation.path];
    assert_eq!(entry["servers"][0]["url"], "/");
    let method = &entry["post"];
    assert_eq!(method["operationId"], "testCalls.probe");
    let parameters = &method["parameters"];
    assert_eq!(parameters[0]["name"], "technology");
    assert_eq!(parameters[1]["name"], "resource");
    assert_eq!(parameters[2]["in"], "query");
    assert_eq!(parameters[2]["name"], "deadline_seconds");
    let schema = &parameters[2]["schema"];
    assert_eq!(schema["type"], "integer");
    assert_eq!(schema["minimum"], 1);
    assert_eq!(schema["maximum"], rusterisk_api::test_call::MAX_SECONDS);
    assert_eq!(schema["default"], rusterisk_api::test_call::DEFAULT_SECONDS);
    assert_eq!(
        method["responses"]["200"]["content"]["application/json"]["schema"]["$ref"],
        "#/components/schemas/TestCallResult"
    );
}

#[test]
fn management_openapi_overrides_the_ari_server_at_its_full_path() {
    let document = openapi();
    let metadata = ManagementOperation::ReloadConfiguration.metadata();
    assert_eq!(document["openapi"], "3.1.0");
    assert_eq!(document["servers"][0]["url"], "/ari");
    assert_eq!(document["servers"].as_array().map(Vec::len), Some(1));
    let path = &document["paths"][metadata.path];
    assert_eq!(path["servers"][0]["url"], "/");
    assert_eq!(path["servers"].as_array().map(Vec::len), Some(1));
    let operation = &path[metadata.method.as_str()];
    assert_eq!(operation["operationId"], metadata.id);
    assert!(operation["responses"].get("204").is_some());
    for status in ["200", "201", "202"] {
        assert!(operation["responses"].get(status).is_none());
    }
    assert!(operation["responses"]["204"].get("content").is_none());
    assert!(operation.get("requestBody").is_none());
    assert!(operation["parameters"]
        .as_array()
        .is_some_and(Vec::is_empty));
    assert!(operation["security"].as_array().is_some_and(|security| {
        security.iter().any(|requirement| {
            requirement["basicAuth"]
                .as_array()
                .is_some_and(Vec::is_empty)
        })
    }));
    assert!(document["paths"]["/channels"].get("servers").is_none());
    assert!(document["paths"]
        .get("/ari/rusterisk/configuration/reload")
        .is_none());
    for operation in OPERATIONS {
        assert_eq!(
            document["paths"][operation.path][operation.method.as_str()]["operationId"],
            operation.id
        );
    }
}

#[test]
fn both_swagger_formats_keep_management_outside_ari_discovery(
) -> Result<(), rusterisk_api::SwaggerError> {
    let metadata = ManagementOperation::ReloadConfiguration.metadata();
    for format in [DiscoveryFormat::Asterisk, DiscoveryFormat::Standard] {
        let resources = resources_for(format);
        let declarations = resources["apis"]
            .as_array()
            .ok_or(rusterisk_api::SwaggerError::Capacity)?;
        assert!(declarations.iter().all(|entry| {
            entry["path"] != "/api-docs/configuration.{format}"
                && entry["path"] != "/api-docs/rusterisk.{format}"
        }));
        let names: std::collections::BTreeSet<_> = OPERATIONS
            .iter()
            .map(rusterisk_api::Operation::resource)
            .collect();
        for resource in names.into_iter().chain(["configuration", "rusterisk"]) {
            let document = swagger_for(resource, format)?;
            assert_eq!(document["basePath"], "/ari");
            let paths = document["apis"]
                .as_array()
                .ok_or(rusterisk_api::SwaggerError::Capacity)?;
            assert!(paths.iter().all(|entry| entry["path"] != metadata.path));
        }
    }
    Ok(())
}
