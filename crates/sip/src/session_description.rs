use crate::{SipError, MAX_MESSAGE};
use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionSecurity {
    Rtp,
    WebRtc(WebRtcDescription),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DtlsSetup {
    ActPass,
    Active,
    Passive,
}

#[derive(Clone, PartialEq, Eq)]
pub struct WebRtcDescription {
    source: String,
    setup: DtlsSetup,
}

fn attribute<'a>(source: &'a str, prefix: &str) -> Result<&'a str, SipError> {
    let (mut session, mut media) = (None, None);
    let mut in_media = false;
    for line in source.lines() {
        if line.starts_with("m=") {
            in_media = true;
        }
        if let Some(value) = line.strip_prefix(prefix) {
            let slot = if in_media { &mut media } else { &mut session };
            if slot.replace(value).is_some() {
                return Err(SipError::InvalidPacket);
            }
        }
    }
    media.or(session).ok_or(SipError::InvalidPacket)
}

fn ice(value: &str, minimum: usize) -> bool {
    (minimum..=256).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"+/".contains(&byte))
}

impl WebRtcDescription {
    pub fn parse(source: &str) -> Result<Self, SipError> {
        if source.len() > MAX_MESSAGE {
            return Err(SipError::Capacity);
        }
        let (mut media, mut candidates, mut lines) = (0, 0, 0);
        for line in source.lines() {
            lines += 1;
            if lines > 256 || line.len() > 2048 {
                return Err(SipError::Capacity);
            }
            if let Some(value) = line.strip_prefix("m=") {
                media += 1;
                let mut fields = value.split_whitespace();
                if fields.next() != Some("audio")
                    || fields.next().is_none()
                    || fields.next() != Some("UDP/TLS/RTP/SAVPF")
                {
                    return Err(SipError::Unsupported);
                }
            }
            if line.starts_with("a=candidate:") {
                candidates += 1;
            }
        }
        if media != 1 {
            return Err(SipError::Unsupported);
        }
        if candidates > 32 {
            return Err(SipError::Capacity);
        }
        if !ice(attribute(source, "a=ice-ufrag:")?, 4) || !ice(attribute(source, "a=ice-pwd:")?, 22)
        {
            return Err(SipError::InvalidPacket);
        }
        let fingerprint = attribute(source, "a=fingerprint:")?
            .strip_prefix("sha-256 ")
            .ok_or(SipError::Unsupported)?;
        let mut octets = 0;
        for octet in fingerprint.split(':') {
            if octet.len() != 2 || !octet.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err(SipError::InvalidPacket);
            }
            octets += 1;
        }
        let setup = match attribute(source, "a=setup:")? {
            "actpass" => DtlsSetup::ActPass,
            "active" => DtlsSetup::Active,
            "passive" => DtlsSetup::Passive,
            _ => return Err(SipError::InvalidPacket),
        };
        if octets != 32 || !source.lines().any(|line| line == "a=rtcp-mux") {
            return Err(SipError::InvalidPacket);
        }
        let mid = attribute(source, "a=mid:")?;
        if mid.is_empty() || mid.len() > 16 || !mid.bytes().all(|byte| byte.is_ascii_alphanumeric())
        {
            return Err(SipError::Unsupported);
        }
        Ok(Self {
            source: source.into(),
            setup,
        })
    }

    pub fn as_str(&self) -> &str {
        &self.source
    }

    pub const fn setup(&self) -> DtlsSetup {
        self.setup
    }
}

impl fmt::Debug for WebRtcDescription {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("WebRtcDescription([redacted])")
    }
}
