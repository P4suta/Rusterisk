use rusterisk_api::{DiscoveryFormat, SwaggerError};
use serde_json::Value;

fn parameter<'a>(parameters: &'a Value, name: &str) -> Result<&'a Value, SwaggerError> {
    parameters
        .as_array()
        .and_then(|parameters| {
            parameters
                .iter()
                .find(|parameter| parameter["name"] == name)
        })
        .ok_or_else(|| SwaggerError::Schema(format!("Missing playback parameter {name}")))
}

#[test]
fn every_playback_route_describes_bounded_offsets_and_the_default_seek_step(
) -> Result<(), SwaggerError> {
    let openapi = rusterisk_api::openapi();
    for (resource, identifier) in [("channels", "channelId"), ("bridges", "bridgeId")] {
        for suffix in ["", "/{playbackId}"] {
            let path = format!("/{resource}/{{{identifier}}}/play{suffix}");
            for (name, default) in [("offsetms", 0), ("skipms", 3000)] {
                let declared = parameter(&openapi["paths"][&path]["post"]["parameters"], name)?;
                assert_eq!(declared["schema"]["type"], "integer");
                assert_eq!(declared["schema"]["format"], "int32");
                assert_eq!(declared["schema"]["minimum"], 0);
                assert_eq!(declared["schema"]["maximum"], i32::MAX);
                assert_eq!(declared["schema"]["default"], default);
                for format in [DiscoveryFormat::Asterisk, DiscoveryFormat::Standard] {
                    let swagger = rusterisk_api::swagger_for(resource, format)?;
                    let operation = swagger["apis"]
                        .as_array()
                        .and_then(|apis| apis.iter().find(|api| api["path"] == path))
                        .ok_or_else(|| SwaggerError::Schema(path.clone()))?;
                    let declared = parameter(&operation["operations"][0]["parameters"], name)?;
                    assert_eq!(declared["type"], "integer");
                    assert_eq!(
                        declared["dataType"],
                        if format == DiscoveryFormat::Asterisk {
                            "int"
                        } else {
                            "integer"
                        }
                    );
                    assert_eq!(declared["minimum"], 0);
                    assert_eq!(declared["maximum"], i32::MAX);
                    assert_eq!(declared["defaultValue"], default);
                }
            }
        }
    }
    Ok(())
}
