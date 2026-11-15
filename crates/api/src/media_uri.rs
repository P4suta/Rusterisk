use crate::{ChannelId, SoundId};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MediaUri {
    Sound(SoundId),
    Recording(ChannelId),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InvalidMediaUri {
    Syntax,
    Name,
    Unsupported,
}

impl std::fmt::Display for InvalidMediaUri {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Syntax => "Media URI requires a scheme and resource name",
            Self::Name => "Invalid media resource name",
            Self::Unsupported => "Unsupported media URI scheme",
        })
    }
}

impl std::error::Error for InvalidMediaUri {}

impl TryFrom<&str> for MediaUri {
    type Error = InvalidMediaUri;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        let (scheme, name) = value.split_once(':').ok_or(InvalidMediaUri::Syntax)?;
        if !matches!(scheme, "sound" | "recording") {
            return Err(InvalidMediaUri::Unsupported);
        }
        if name.len() > 128 {
            return Err(InvalidMediaUri::Name);
        }
        match scheme {
            "sound" => SoundId::try_from(name.to_owned())
                .map(Self::Sound)
                .map_err(|_| InvalidMediaUri::Name),
            "recording" => ChannelId::try_from(name.to_owned())
                .map(Self::Recording)
                .map_err(|_| InvalidMediaUri::Name),
            _ => Err(InvalidMediaUri::Unsupported),
        }
    }
}

impl std::fmt::Display for MediaUri {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (scheme, name) = match self {
            Self::Sound(name) => ("sound", name.as_str()),
            Self::Recording(name) => ("recording", name.as_str()),
        };
        write!(f, "{scheme}:{name}")
    }
}
