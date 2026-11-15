use crate::EventFilter;
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EventFilterRequest {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_filters"
    )]
    #[schemars(with = "Vec<EventFilter>")]
    pub allowed: Option<Vec<EventFilter>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_filters"
    )]
    #[schemars(with = "Vec<EventFilter>")]
    pub disallowed: Option<Vec<EventFilter>>,
}

fn present_filters<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Vec<EventFilter>>, D::Error> {
    Vec::<EventFilter>::deserialize(deserializer).map(Some)
}
