use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::net::{IpAddr, SocketAddr};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum StaticRtpCodec {
    #[serde(rename = "ulaw")]
    Pcmu,
    #[serde(rename = "alaw")]
    Pcma,
}

impl StaticRtpCodec {
    pub const fn payload_type(self) -> u8 {
        match self {
            Self::Pcmu => 0,
            Self::Pcma => 8,
        }
    }
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pcmu => "ulaw",
            Self::Pcma => "alaw",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExternalRtpTarget {
    remote: SocketAddr,
    codec: StaticRtpCodec,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidExternalRtpTarget;

impl std::fmt::Display for InvalidExternalRtpTarget {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .write_str("External media requires a unicast address and a port from 1 through 65534")
    }
}
impl std::error::Error for InvalidExternalRtpTarget {}

impl ExternalRtpTarget {
    pub fn new(
        remote: SocketAddr,
        codec: StaticRtpCodec,
    ) -> Result<Self, InvalidExternalRtpTarget> {
        let broadcast = match remote.ip() {
            IpAddr::V4(ip) => ip.is_broadcast(),
            IpAddr::V6(_) => false,
        };
        if remote.port() == 0
            || remote.port() == u16::MAX
            || remote.ip().is_unspecified()
            || remote.ip().is_multicast()
            || broadcast
        {
            return Err(InvalidExternalRtpTarget);
        }
        Ok(Self { remote, codec })
    }
    pub const fn remote(self) -> SocketAddr {
        self.remote
    }
    pub const fn codec(self) -> StaticRtpCodec {
        self.codec
    }
}
