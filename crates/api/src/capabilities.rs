use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct Capabilities {
    pub reference: String,
    pub operations: Vec<OperationCapability>,
    #[serde(default)]
    pub verified_call_paths: Vec<VerifiedCallPath>,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct OperationCapability {
    pub operation: String,
    pub call_verified: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct VerifiedCallPath {
    pub path: String,
    pub signaling: String,
    pub media: String,
    pub codec: String,
    pub conditions: Vec<String>,
}
