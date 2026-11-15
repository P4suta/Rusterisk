use crate::{ChannelId, MediaControlFormat, MediaEvent, MediaFormat};
use serde::Deserialize;
use std::{collections::BTreeMap, str::FromStr};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaEventError {
    Capacity,
    Invalid,
    ForeignChannel,
}

impl std::fmt::Display for MediaEventError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Capacity => "media event capacity exceeded",
            Self::Invalid => "invalid media event",
            Self::ForeignChannel => "media event belongs to another channel",
        })
    }
}
impl std::error::Error for MediaEventError {}

struct Fields<'a>(BTreeMap<&'a str, &'a str>);

impl<'a> Fields<'a> {
    fn new(text: &'a str) -> Result<Self, MediaEventError> {
        let mut fields = BTreeMap::new();
        for token in text.split_ascii_whitespace() {
            let (name, value) = token.split_once(':').ok_or(MediaEventError::Invalid)?;
            if fields.len() >= 16 || name.len() > 64 || value.len() > 4096 {
                return Err(MediaEventError::Capacity);
            }
            if name.is_empty() || fields.insert(name, value).is_some() {
                return Err(MediaEventError::Invalid);
            }
        }
        Ok(Self(fields))
    }
    fn get(&self, name: &str) -> Result<&'a str, MediaEventError> {
        self.0.get(name).copied().ok_or(MediaEventError::Invalid)
    }
    fn number<T: FromStr>(&self, name: &str) -> Result<T, MediaEventError> {
        self.get(name)?
            .parse()
            .map_err(|_| MediaEventError::Invalid)
    }
    fn id(&self, name: &str) -> Result<ChannelId, MediaEventError> {
        ChannelId::try_from(self.get(name)?.to_owned()).map_err(|_| MediaEventError::Invalid)
    }
}

fn plain(text: &str, context: &ChannelId) -> Result<MediaEvent, MediaEventError> {
    let (name, data) = text.split_once(' ').unwrap_or((text, ""));
    if matches!(name, "MEDIA_XOFF" | "MEDIA_XON" | "QUEUE_DRAINED") && !data.is_empty() {
        return Err(MediaEventError::Invalid);
    }
    match name {
        "MEDIA_XOFF" => Ok(MediaEvent::MediaXoff {
            channel_id: context.clone(),
        }),
        "MEDIA_XON" => Ok(MediaEvent::MediaXon {
            channel_id: context.clone(),
        }),
        "QUEUE_DRAINED" => Ok(MediaEvent::QueueDrained {
            channel_id: context.clone(),
        }),
        "MEDIA_BUFFERING_COMPLETED" => Ok(MediaEvent::MediaBufferingCompleted {
            channel_id: context.clone(),
            correlation_id: data.into(),
        }),
        "MEDIA_MARK_PROCESSED" => Ok(MediaEvent::MediaMarkProcessed {
            channel_id: context.clone(),
            correlation_id: data.into(),
        }),
        "ERROR" => {
            let (fields, error_text) = data
                .split_once(" error_text:")
                .ok_or(MediaEventError::Invalid)?;
            Ok(MediaEvent::Error {
                channel_id: Fields::new(fields)?.id("channel_id")?,
                error_text: error_text.into(),
            })
        }
        "DTMF_END" => {
            let fields = Fields::new(data)?;
            let digit = fields.get("digit")?;
            if digit.len() != 1 {
                return Err(MediaEventError::Invalid);
            }
            Ok(MediaEvent::DtmfEnd {
                channel_id: fields.id("channel_id")?,
                digit: char::from(digit.as_bytes()[0]),
            })
        }
        "MEDIA_START" => {
            let fields = Fields::new(data)?;
            let format = MediaFormat::deserialize(serde::de::value::StrDeserializer::<
                serde::de::value::Error,
            >::new(fields.get("format")?))
            .map_err(|_| MediaEventError::Invalid)?;
            Ok(MediaEvent::MediaStart {
                connection_id: fields.id("connection_id")?,
                channel: fields.get("channel")?.into(),
                channel_id: fields.id("channel_id")?,
                format,
                optimal_frame_size: fields.number("optimal_frame_size")?,
                ptime: fields.number("ptime")?,
                channel_variables: BTreeMap::new(),
            })
        }
        "STATUS" => {
            let fields = Fields::new(data)?;
            Ok(MediaEvent::Status {
                channel_id: fields.id("channel_id")?,
                queue_length: fields.number("queue_length")?,
                xon_level: fields.number("xon_level")?,
                xoff_level: fields.number("xoff_level")?,
                queue_full: fields.number("queue_full")?,
                bulk_media: fields.number("bulk_media")?,
                media_paused: fields.number("media_paused")?,
            })
        }
        _ => Err(MediaEventError::Invalid),
    }
}

impl MediaEvent {
    pub fn parse(
        text: &str,
        format: MediaControlFormat,
        context: &ChannelId,
    ) -> Result<Self, MediaEventError> {
        if text.len() > 65500 {
            return Err(MediaEventError::Capacity);
        }
        let event = match format {
            MediaControlFormat::PlainText => plain(text, context)?,
            MediaControlFormat::Json => {
                serde_json::from_str(text).map_err(|_| MediaEventError::Invalid)?
            }
        };
        let channel = match &event {
            Self::MediaStart { channel_id, .. }
            | Self::DtmfEnd { channel_id, .. }
            | Self::MediaXoff { channel_id }
            | Self::MediaXon { channel_id }
            | Self::QueueDrained { channel_id }
            | Self::MediaBufferingCompleted { channel_id, .. }
            | Self::MediaMarkProcessed { channel_id, .. }
            | Self::Status { channel_id, .. }
            | Self::Error { channel_id, .. } => channel_id,
        };
        if channel != context {
            return Err(MediaEventError::ForeignChannel);
        }
        match &event {
            Self::MediaStart {
                channel,
                channel_variables,
                ..
            } => {
                if channel.len() > 256
                    || channel_variables.len() > 128
                    || channel_variables
                        .iter()
                        .any(|(name, value)| name.len() > 128 || value.len() > 4096)
                {
                    return Err(MediaEventError::Capacity);
                }
                if channel.chars().any(char::is_control) {
                    return Err(MediaEventError::Invalid);
                }
            }
            Self::DtmfEnd { digit, .. } => {
                if !"0123456789*#ABCD".contains(*digit) {
                    return Err(MediaEventError::Invalid);
                }
            }
            Self::MediaBufferingCompleted { correlation_id, .. }
            | Self::MediaMarkProcessed { correlation_id, .. } => {
                if correlation_id.len() > 128 {
                    return Err(MediaEventError::Capacity);
                }
                if correlation_id.chars().any(char::is_control) {
                    return Err(MediaEventError::Invalid);
                }
            }
            Self::Error { error_text, .. } => {
                if error_text.len() > 4096 {
                    return Err(MediaEventError::Capacity);
                }
            }
            Self::MediaXoff { .. }
            | Self::MediaXon { .. }
            | Self::QueueDrained { .. }
            | Self::Status { .. } => {}
        }
        Ok(event)
    }
}
