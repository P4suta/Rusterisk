use crate::{ChannelId, MediaControlFormat};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum MediaFormat {
    #[serde(rename = "ulaw")]
    Pcmu,
    #[serde(rename = "alaw")]
    Pcma,
    #[serde(rename = "slin")]
    Pcm8,
    #[serde(rename = "slin12")]
    Pcm12,
    #[serde(rename = "slin16")]
    Pcm16,
    #[serde(rename = "slin24")]
    Pcm24,
    #[serde(rename = "slin32")]
    Pcm32,
    #[serde(rename = "slin44")]
    Pcm44,
    #[serde(rename = "slin48")]
    Pcm48,
    #[serde(rename = "opus")]
    Opus,
}

impl MediaFormat {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pcmu => "ulaw",
            Self::Pcma => "alaw",
            Self::Pcm8 => "slin",
            Self::Pcm12 => "slin12",
            Self::Pcm16 => "slin16",
            Self::Pcm24 => "slin24",
            Self::Pcm32 => "slin32",
            Self::Pcm44 => "slin44",
            Self::Pcm48 => "slin48",
            Self::Opus => "opus",
        }
    }
    pub const fn rate(self) -> u32 {
        match self {
            Self::Pcmu | Self::Pcma | Self::Pcm8 => 8000,
            Self::Pcm12 => 12000,
            Self::Pcm16 => 16000,
            Self::Pcm24 => 24000,
            Self::Pcm32 => 32000,
            Self::Pcm44 => 44100,
            Self::Pcm48 | Self::Opus => 48000,
        }
    }
    pub const fn optimal_frame_size(self) -> u16 {
        match self {
            Self::Pcmu | Self::Pcma => 160,
            Self::Opus => 0,
            Self::Pcm8
            | Self::Pcm12
            | Self::Pcm16
            | Self::Pcm24
            | Self::Pcm32
            | Self::Pcm44
            | Self::Pcm48 => (self.rate() / 50 * 2) as u16,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "event", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum MediaEvent {
    MediaStart {
        connection_id: ChannelId,
        channel: String,
        channel_id: ChannelId,
        format: MediaFormat,
        optimal_frame_size: u16,
        ptime: u16,
        channel_variables: BTreeMap<String, String>,
    },
    DtmfEnd {
        channel_id: ChannelId,
        digit: char,
    },
    MediaXoff {
        channel_id: ChannelId,
    },
    MediaXon {
        channel_id: ChannelId,
    },
    QueueDrained {
        channel_id: ChannelId,
    },
    MediaBufferingCompleted {
        channel_id: ChannelId,
        correlation_id: String,
    },
    MediaMarkProcessed {
        channel_id: ChannelId,
        correlation_id: String,
    },
    Status {
        channel_id: ChannelId,
        queue_length: u16,
        xon_level: u16,
        xoff_level: u16,
        queue_full: bool,
        bulk_media: bool,
        media_paused: bool,
    },
    Error {
        channel_id: ChannelId,
        error_text: String,
    },
}

impl MediaEvent {
    pub fn encode(&self, format: MediaControlFormat) -> Result<String, serde_json::Error> {
        match format {
            MediaControlFormat::Json => serde_json::to_string(self),
            MediaControlFormat::PlainText => Ok(match self {
                Self::MediaStart { connection_id, channel, channel_id, format, optimal_frame_size, ptime, .. } => format!("MEDIA_START connection_id:{connection_id} channel:{channel} channel_id:{channel_id} format:{} optimal_frame_size:{optimal_frame_size} ptime:{ptime}", format.as_str()),
                Self::DtmfEnd { channel_id, digit } => format!("DTMF_END digit:{digit} channel_id:{channel_id}"),
                Self::MediaXoff { .. } => "MEDIA_XOFF".into(),
                Self::MediaXon { .. } => "MEDIA_XON".into(),
                Self::QueueDrained { .. } => "QUEUE_DRAINED".into(),
                Self::MediaBufferingCompleted { correlation_id, .. } => correlated("MEDIA_BUFFERING_COMPLETED", correlation_id),
                Self::MediaMarkProcessed { correlation_id, .. } => correlated("MEDIA_MARK_PROCESSED", correlation_id),
                Self::Status { channel_id, queue_length, xon_level, xoff_level, queue_full, bulk_media, media_paused } => format!("STATUS channel_id:{channel_id} queue_length:{queue_length} xon_level:{xon_level} xoff_level:{xoff_level} queue_full:{queue_full} bulk_media:{bulk_media} media_paused:{media_paused}"),
                Self::Error { channel_id, error_text } => format!("ERROR channel_id:{channel_id} error_text:{error_text}"),
            }),
        }
    }
}

fn correlated(name: &str, id: &str) -> String {
    if id.is_empty() {
        name.into()
    } else {
        format!("{name} {id}")
    }
}
