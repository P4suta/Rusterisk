use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum MediaDirection {
    Both,
    In,
    Out,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum MediaControlFormat {
    #[serde(rename = "plain-text")]
    PlainText,
    #[serde(rename = "json")]
    Json,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "command", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum MediaCommand {
    Answer,
    Hangup,
    StartMediaBuffering,
    StopMediaBuffering {
        #[serde(skip_serializing_if = "Option::is_none")]
        correlation_id: Option<String>,
    },
    FlushMedia,
    PauseMedia,
    ContinueMedia,
    MarkMedia {
        #[serde(skip_serializing_if = "Option::is_none")]
        correlation_id: Option<String>,
    },
    GetStatus,
    ReportQueueDrained,
    SetMediaDirection {
        direction: MediaDirection,
    },
}

#[derive(Debug)]
pub enum MediaCommandError {
    Capacity,
    Invalid,
    Json(serde_json::Error),
}

impl std::fmt::Display for MediaCommandError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Capacity => formatter.write_str("media command capacity exceeded"),
            Self::Invalid => formatter.write_str("invalid media command"),
            Self::Json(error) => error.fmt(formatter),
        }
    }
}
impl std::error::Error for MediaCommandError {}

impl MediaCommand {
    fn validate(&self) -> Result<(), MediaCommandError> {
        match self {
            Self::StopMediaBuffering { correlation_id } | Self::MarkMedia { correlation_id } => {
                if let Some(id) = correlation_id {
                    if id.len() > 128 {
                        return Err(MediaCommandError::Capacity);
                    }
                    if id.chars().any(char::is_control) {
                        return Err(MediaCommandError::Invalid);
                    }
                }
            }
            Self::Answer
            | Self::Hangup
            | Self::StartMediaBuffering
            | Self::FlushMedia
            | Self::PauseMedia
            | Self::ContinueMedia
            | Self::GetStatus
            | Self::ReportQueueDrained
            | Self::SetMediaDirection { .. } => {}
        }
        Ok(())
    }

    pub fn parse(text: &str, format: MediaControlFormat) -> Result<Self, MediaCommandError> {
        let command = match format {
            MediaControlFormat::Json => {
                if text.len() > 16384 {
                    return Err(MediaCommandError::Capacity);
                }
                serde_json::from_str(text).map_err(MediaCommandError::Json)?
            }
            MediaControlFormat::PlainText => {
                if text.len() > 128 {
                    return Err(MediaCommandError::Capacity);
                }
                if !text.is_ascii() || text.chars().any(char::is_control) {
                    return Err(MediaCommandError::Invalid);
                }
                let (name, data) = text.split_once(' ').unwrap_or((text, ""));
                match (name, data) {
                    ("ANSWER", "") => Self::Answer,
                    ("HANGUP", "") => Self::Hangup,
                    ("START_MEDIA_BUFFERING", "") => Self::StartMediaBuffering,
                    ("STOP_MEDIA_BUFFERING", data) => Self::StopMediaBuffering {
                        correlation_id: (!data.is_empty()).then(|| data.to_owned()),
                    },
                    ("FLUSH_MEDIA", "") => Self::FlushMedia,
                    ("PAUSE_MEDIA", "") => Self::PauseMedia,
                    ("CONTINUE_MEDIA", "") => Self::ContinueMedia,
                    ("MARK_MEDIA", data) => Self::MarkMedia {
                        correlation_id: (!data.is_empty()).then(|| data.to_owned()),
                    },
                    ("GET_STATUS", "") => Self::GetStatus,
                    ("REPORT_QUEUE_DRAINED", "") => Self::ReportQueueDrained,
                    ("SET_MEDIA_DIRECTION", "both") => Self::SetMediaDirection {
                        direction: MediaDirection::Both,
                    },
                    ("SET_MEDIA_DIRECTION", "in") => Self::SetMediaDirection {
                        direction: MediaDirection::In,
                    },
                    ("SET_MEDIA_DIRECTION", "out") => Self::SetMediaDirection {
                        direction: MediaDirection::Out,
                    },
                    _ => return Err(MediaCommandError::Invalid),
                }
            }
        };
        command.validate()?;
        Ok(command)
    }

    pub fn encode(&self, format: MediaControlFormat) -> Result<String, MediaCommandError> {
        self.validate()?;
        match format {
            MediaControlFormat::Json => {
                serde_json::to_string(self).map_err(MediaCommandError::Json)
            }
            MediaControlFormat::PlainText => {
                let (name, data) = match self {
                    Self::Answer => ("ANSWER", None),
                    Self::Hangup => ("HANGUP", None),
                    Self::StartMediaBuffering => ("START_MEDIA_BUFFERING", None),
                    Self::StopMediaBuffering { correlation_id } => {
                        ("STOP_MEDIA_BUFFERING", correlation_id.as_deref())
                    }
                    Self::FlushMedia => ("FLUSH_MEDIA", None),
                    Self::PauseMedia => ("PAUSE_MEDIA", None),
                    Self::ContinueMedia => ("CONTINUE_MEDIA", None),
                    Self::MarkMedia { correlation_id } => ("MARK_MEDIA", correlation_id.as_deref()),
                    Self::GetStatus => ("GET_STATUS", None),
                    Self::ReportQueueDrained => ("REPORT_QUEUE_DRAINED", None),
                    Self::SetMediaDirection { direction } => (
                        "SET_MEDIA_DIRECTION",
                        Some(match direction {
                            MediaDirection::Both => "both",
                            MediaDirection::In => "in",
                            MediaDirection::Out => "out",
                        }),
                    ),
                };
                let text = match data {
                    Some(data) if !data.is_empty() => format!("{name} {data}"),
                    Some(_) | None => name.to_owned(),
                };
                if text.len() > 128 {
                    return Err(MediaCommandError::Capacity);
                }
                if !text.is_ascii() {
                    return Err(MediaCommandError::Invalid);
                }
                Ok(text)
            }
        }
    }
}
