use std::net::{IpAddr, SocketAddr};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AudioSocketTarget {
    remote: SocketAddr,
    session: [u8; 16],
    session_text: [u8; 36],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidAudioSocketTarget;

impl std::fmt::Display for InvalidAudioSocketTarget {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("AudioSocket requires a unicast address and a nonzero port")
    }
}
impl std::error::Error for InvalidAudioSocketTarget {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidAudioSocketSession;

impl std::fmt::Display for InvalidAudioSocketSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("AudioSocket requires a matching hyphenated UUID")
    }
}
impl std::error::Error for InvalidAudioSocketSession {}

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
        let mut session_text = [b'-'; 36];
        let mut offset = 0;
        let hex = b"0123456789abcdef";
        for byte in session {
            if matches!(offset, 8 | 13 | 18 | 23) {
                offset += 1;
            }
            session_text[offset] = hex[usize::from(byte >> 4)];
            session_text[offset + 1] = hex[usize::from(byte & 15)];
            offset += 2;
        }
        Ok(Self {
            remote,
            session,
            session_text,
        })
    }
    pub fn with_session_text(mut self, text: &str) -> Result<Self, InvalidAudioSocketSession> {
        if !self.session_text.eq_ignore_ascii_case(text.as_bytes()) {
            return Err(InvalidAudioSocketSession);
        }
        self.session_text.copy_from_slice(text.as_bytes());
        Ok(self)
    }
    pub fn session_text(self) -> String {
        self.session_text.into_iter().map(char::from).collect()
    }
    pub const fn remote(self) -> SocketAddr {
        self.remote
    }
    pub const fn session(self) -> [u8; 16] {
        self.session
    }
}
