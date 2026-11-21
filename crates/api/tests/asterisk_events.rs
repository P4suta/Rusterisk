use rusterisk_api::{schemas, Event};
use serde_json::json;
use std::collections::BTreeSet;

#[test]
fn all_published_event_types_have_a_native_model_or_preserve_their_original_fields(
) -> Result<(), String> {
    let names: BTreeSet<_> = include_str!("fixtures/asterisk-22.9.0-events.txt")
        .lines()
        .collect();
    let model = schemas();
    let modeled: BTreeSet<_> = model["Event"]["oneOf"]
        .as_array()
        .ok_or("missing Event schema")?
        .iter()
        .map(|branch| {
            branch["properties"]["type"]["const"]
                .as_str()
                .ok_or("missing discriminator")
        })
        .collect::<Result<_, _>>()?;
    assert!(modeled.is_subset(&names));
    let participants = ["first", "second"];
    for &name in names.difference(&modeled) {
        let value = json!({"type":name,"application":"calls","timestamp":"now","asterisk_id":"pbx",
            "details":{"participants":participants,"accepted":true}});
        let event: Event =
            serde_json::from_value(value.clone()).map_err(|error| format!("{name}: {error}"))?;
        assert_eq!(event.kind.event_type(), name);
        assert_eq!(
            serde_json::to_value(event).map_err(|error| error.to_string())?,
            value
        );
    }
    for invalid in [
        json!({"type":"ChannelDialplan","application":"calls","timestamp":"now"}),
        json!({"type":"UnrecognizedEvent","application":"calls","timestamp":"now"}),
        json!({"type":"ApplicationRegistered","application":"calls"}),
    ] {
        assert!(serde_json::from_value::<Event>(invalid).is_err());
    }
    Ok(())
}
