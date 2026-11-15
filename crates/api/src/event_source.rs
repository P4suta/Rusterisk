use crate::ChannelId;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum EventSource {
    AllChannels,
    Channel(ChannelId),
    AllBridges,
    Bridge(ChannelId),
    AllEndpoints,
    EndpointTechnology(ChannelId),
    Endpoint {
        technology: ChannelId,
        resource: ChannelId,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InvalidEventSource {
    Scheme,
    Resource,
}

impl std::fmt::Display for InvalidEventSource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Scheme => "Invalid event source URI scheme",
            Self::Resource => "Invalid event source resource",
        })
    }
}
impl std::error::Error for InvalidEventSource {}

fn identifier(value: &str) -> Result<ChannelId, InvalidEventSource> {
    if value.len() > 128 {
        return Err(InvalidEventSource::Resource);
    }
    ChannelId::try_from(value.to_owned()).map_err(|_| InvalidEventSource::Resource)
}

impl TryFrom<&str> for EventSource {
    type Error = InvalidEventSource;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        let (scheme, resource) = value.split_once(':').ok_or(InvalidEventSource::Scheme)?;
        match (scheme, resource) {
            ("channel", "") => Ok(Self::AllChannels),
            ("channel", resource) => Ok(Self::Channel(identifier(resource)?)),
            ("bridge", "") => Ok(Self::AllBridges),
            ("bridge", resource) => Ok(Self::Bridge(identifier(resource)?)),
            ("endpoint", "") => Ok(Self::AllEndpoints),
            ("endpoint", resource) => match resource.split_once('/') {
                Some((technology, resource)) => Ok(Self::Endpoint {
                    technology: identifier(technology)?,
                    resource: identifier(resource)?,
                }),
                None => Ok(Self::EndpointTechnology(identifier(resource)?)),
            },
            _ => Err(InvalidEventSource::Scheme),
        }
    }
}

impl std::fmt::Display for EventSource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AllChannels => formatter.write_str("channel:"),
            Self::Channel(id) => write!(formatter, "channel:{id}"),
            Self::AllBridges => formatter.write_str("bridge:"),
            Self::Bridge(id) => write!(formatter, "bridge:{id}"),
            Self::AllEndpoints => formatter.write_str("endpoint:"),
            Self::EndpointTechnology(technology) => write!(formatter, "endpoint:{technology}"),
            Self::Endpoint {
                technology,
                resource,
            } => write!(formatter, "endpoint:{technology}/{resource}"),
        }
    }
}
