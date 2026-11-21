use rusterisk_api::{ari_route, is_known_operation, AriRoute, OPERATIONS};
use std::collections::BTreeSet;

const REFERENCE: &str = include_str!("fixtures/asterisk-22.9.0-routes.tsv");
const PARAMETERS: &str = include_str!("fixtures/asterisk-22.9.0-parameters.tsv");

#[test]
fn asterisk_discovery_preserves_published_parameter_types_locations_and_cardinality(
) -> Result<(), String> {
    let resources: BTreeSet<_> = OPERATIONS
        .iter()
        .map(|operation| operation.resource())
        .collect();
    let mut published: BTreeSet<_> = PARAMETERS.lines().collect();
    assert_eq!(
        published.len(),
        PARAMETERS.lines().count(),
        "duplicate reference parameter"
    );
    let openapi = rusterisk_api::openapi();
    for resource in resources {
        let discovery =
            rusterisk_api::swagger_for(resource, rusterisk_api::DiscoveryFormat::Asterisk)
                .map_err(|error| error.to_string())?;
        for path in discovery["apis"]
            .as_array()
            .ok_or("missing discovery paths")?
        {
            let template = path["path"].as_str().ok_or("missing path")?;
            for operation in path["operations"].as_array().ok_or("missing operations")? {
                let method = operation["httpMethod"].as_str().ok_or("missing method")?;
                for parameter in operation["parameters"]
                    .as_array()
                    .ok_or("missing parameters")?
                {
                    let name = parameter["name"].as_str().ok_or("missing name")?;
                    let location = parameter["paramType"].as_str().ok_or("missing location")?;
                    let kind = parameter["dataType"].as_str().ok_or("missing type")?;
                    let mut required = parameter["required"]
                        .as_bool()
                        .ok_or("missing required flag")?;
                    let mut multiple = parameter["allowMultiple"]
                        .as_bool()
                        .ok_or("missing multiplicity")?;
                    if (template, name) == ("/events", "app") {
                        assert!(!multiple, "one application per native session");
                        multiple = true;
                    }
                    if (template, name) == ("/channels/{channelId}/dtmf", "dtmf") {
                        assert!(required, "sending digits requires a nonempty string");
                        required = false;
                    }
                    let row = format!(
                        "{method}\t{template}\t{name}\t{location}\t{kind}\t{required}\t{multiple}"
                    );
                    assert!(published.remove(row.as_str()), "unmatched parameter {row}");
                    if let Some(width) = match kind {
                        "int" => Some("int32"),
                        "long" => Some("int64"),
                        _ => None,
                    } {
                        let definition = &openapi["paths"][template][method.to_ascii_lowercase()];
                        let schema = definition["parameters"]
                            .as_array()
                            .and_then(|values| values.iter().find(|item| item["name"] == name))
                            .ok_or("missing OpenAPI parameter")?;
                        assert_eq!(
                            schema["schema"]["format"], width,
                            "{method} {template} {name}"
                        );
                        if definition.get("requestBody").is_some() {
                            assert_eq!(
                                definition["requestBody"]["content"]["application/json"]["schema"]
                                    ["properties"][name]["format"],
                                width
                            );
                        }
                    }
                }
            }
        }
    }
    for op in OPERATIONS {
        for &(name, kind) in op.unavailable_parameters() {
            let data_type = match kind {
                rusterisk_api::UnavailableParameterKind::String => "string",
                rusterisk_api::UnavailableParameterKind::Boolean => "boolean",
            };
            let row = format!(
                "{}\t{}\t{name}\tquery\t{data_type}\tfalse\tfalse",
                op.method.upper(),
                op.path
            );
            assert!(
                published.remove(row.as_str()),
                "unmatched unavailable parameter {row}"
            );
        }
    }
    for row in published {
        let mut fields = row.split('\t');
        let method = fields.next().ok_or("missing method")?;
        let path = fields.next().ok_or("missing path")?;
        assert!(
            !OPERATIONS
                .iter()
                .any(|op| op.method.upper() == method && op.path == path),
            "unclassified parameter {row}"
        );
    }
    Ok(())
}

#[test]
fn every_published_ari_route_has_an_explicit_disposition() -> Result<(), String> {
    let mut identities = BTreeSet::new();
    for row in REFERENCE.lines() {
        let fields: Vec<_> = row.split('\t').collect();
        assert_eq!(fields.len(), 4, "{row}");
        let (method, template, nickname) = (fields[0], fields[1], fields[2]);
        assert!(identities.insert((method, template, nickname)), "{row}");
        let concrete = template.split('/').skip(1).map(|segment| {
            if segment.starts_with('{') {
                "sample"
            } else {
                segment
            }
        });
        match ari_route(method, concrete) {
            AriRoute::Declared(op) => {
                assert_eq!(
                    (op.method.upper(), op.path, op.nickname),
                    (method, template, nickname)
                );
                if op.id == "events.eventWebsocket" {
                    assert_eq!((op.response, fields[3]), ("Event", "Message"));
                } else if let Some(item) = op.response.strip_suffix("[]") {
                    assert_eq!(format!("List[{item}]"), fields[3], "{row}");
                } else {
                    assert_eq!(op.response, fields[3], "{row}");
                }
            }
            AriRoute::Unsupported => assert!(
                !OPERATIONS
                    .iter()
                    .any(|op| op.method.upper() == method && op.path == template),
                "Declared operation was routed to an unavailable adapter: {row}"
            ),
            AriRoute::OtherMethods(_) | AriRoute::NotFound => {
                return Err(format!("Unclassified Asterisk operation: {row}"));
            }
        }
    }
    for op in OPERATIONS {
        assert!(
            identities.contains(&(op.method.upper(), op.path, op.nickname)),
            "{}",
            op.id
        );
    }
    Ok(())
}

#[test]
fn reserved_ari_paths_do_not_fall_back_to_channel_identifiers() {
    assert!(!is_known_operation("DELETE", "/channels/create"));
    assert!(!is_known_operation(
        "POST",
        "/channels/externalMedia/answer"
    ));
    assert!(is_known_operation("POST", "/channels/sample/answer"));
}
