use crate::{MediaControlFormat, MediaDirection, MediaFormat};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WebSocketMediaOptions {
    pub format: MediaFormat,
    pub control_format: MediaControlFormat,
    pub direction: MediaDirection,
    pub auto_answer: bool,
}

impl WebSocketMediaOptions {
    pub const fn new(format: MediaFormat) -> Self {
        Self {
            format,
            control_format: MediaControlFormat::PlainText,
            direction: MediaDirection::Both,
            auto_answer: true,
        }
    }

    pub fn transport_data(self) -> String {
        let control = match self.control_format {
            MediaControlFormat::PlainText => "plain-text",
            MediaControlFormat::Json => "json",
        };
        format!("f({control}){}", if self.auto_answer { "" } else { "n" })
    }
}
