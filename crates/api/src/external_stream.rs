use std::net::{IpAddr, SocketAddr};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AudioSocketTarget {
    remote: SocketAddr,
    session: [u8; 16],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidAudioSocketTarget;

impl std::fmt::Display for InvalidAudioSocketTarget {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("AudioSocket requires a unicast address and a nonzero port")
    }
}
impl std::error::Error for InvalidAudioSocketTarget {}

impl AudioSocketTarget {
    pub fn new(remote: SocketAddr, session: [u8; 16]) -> Result<Self, InvalidAudioSocketTarget> {
        let broadcast = match remote.ip() {
            IpAddr::V4(ip) => ip.is_broadcast(),
            IpAddr::V6(_) => false,
        };
        if remote.port() == 0
            || remote.ip().is_unspecified()
            || remote.ip().is_multicast()
            || broadcast
        {
            return Err(InvalidAudioSocketTarget);
        }
        Ok(Self { remote, session })
    }
    pub const fn remote(self) -> SocketAddr {
        self.remote
    }
    pub const fn session(self) -> [u8; 16] {
        self.session
    }
}
