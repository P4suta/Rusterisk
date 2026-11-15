use crate::SipError;

enum Fingerprints<'a> {
    Absent,
    Other,
    Sha256(&'a str),
    Alternatives,
}

impl<'a> Fingerprints<'a> {
    fn observe(&mut self, value: &'a str) -> Result<(), SipError> {
        let (hash, fingerprint) = value.split_once(' ').ok_or(SipError::InvalidPacket)?;
        if hash.is_empty()
            || !hash
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Err(SipError::InvalidPacket);
        }
        let mut octets = 0;
        for octet in fingerprint.split(':') {
            if octet.len() != 2 || !octet.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err(SipError::InvalidPacket);
            }
            octets += 1;
        }
        let expected = match hash.to_ascii_lowercase().as_str() {
            "sha-1" => Some(20),
            "sha-224" => Some(28),
            "sha-256" => Some(32),
            "sha-384" => Some(48),
            "sha-512" => Some(64),
            "md2" | "md5" => Some(16),
            _ => None,
        };
        if expected.is_some_and(|expected| expected != octets) {
            return Err(SipError::InvalidPacket);
        }
        if hash.eq_ignore_ascii_case("sha-256") {
            match self {
                Self::Absent | Self::Other => *self = Self::Sha256(fingerprint),
                Self::Sha256(previous) if !previous.eq_ignore_ascii_case(fingerprint) => {
                    *self = Self::Alternatives
                }
                Self::Sha256(_) | Self::Alternatives => {}
            }
        } else if matches!(self, Self::Absent) {
            *self = Self::Other;
        }
        Ok(())
    }
}

pub(super) fn validate(source: &str) -> Result<(), SipError> {
    let (mut session, mut media) = (Fingerprints::Absent, Fingerprints::Absent);
    let mut in_media = false;
    for line in source.lines() {
        if line.starts_with("m=") {
            in_media = true;
        }
        if let Some(value) = line.strip_prefix("a=fingerprint:") {
            if in_media {
                media.observe(value)?;
            } else {
                session.observe(value)?;
            }
        }
    }
    let effective = if matches!(media, Fingerprints::Absent) {
        session
    } else {
        media
    };
    match effective {
        Fingerprints::Sha256(_) => Ok(()),
        Fingerprints::Other | Fingerprints::Alternatives => Err(SipError::Unsupported),
        Fingerprints::Absent => Err(SipError::InvalidPacket),
    }
}
