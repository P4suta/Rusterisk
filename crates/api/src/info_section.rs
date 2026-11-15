use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum InfoSection {
    Build,
    System,
    Config,
    Status,
}

impl InfoSection {
    pub const ALL: [Self; 4] = [Self::Build, Self::System, Self::Config, Self::Status];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Build => "build",
            Self::System => "system",
            Self::Config => "config",
            Self::Status => "status",
        }
    }
}
