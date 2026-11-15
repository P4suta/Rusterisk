use crate::{Direction, DtlsSetup, Session, SessionSecurity, SipError, WebRtcDescription};
use std::net::SocketAddr;

fn origin(source: &str) -> Result<([&str; 5], u64), SipError> {
    let mut origins = source.lines().filter_map(|line| line.strip_prefix("o="));
    let line = origins.next().ok_or(SipError::InvalidPacket)?;
    if origins.next().is_some() {
        return Err(SipError::InvalidPacket);
    }
    let fields: Vec<&str> = line.split_whitespace().take(7).collect();
    let [user, id, version, network, family, address] = fields.as_slice() else {
        return Err(SipError::InvalidPacket);
    };
    id.parse::<i64>()
        .ok()
        .filter(|id| *id >= 0)
        .ok_or(SipError::InvalidPacket)?;
    let version = version
        .parse::<i64>()
        .ok()
        .filter(|version| *version >= 0)
        .ok_or(SipError::InvalidPacket)?;
    Ok(([*user, *id, *network, *family, *address], version as u64))
}

fn unchanged(line: &&str, webrtc: bool) -> bool {
    !line.starts_with("o=")
        && !(webrtc && line.starts_with("a=setup:"))
        && !matches!(
            *line,
            "a=sendrecv" | "a=sendonly" | "a=recvonly" | "a=inactive"
        )
}

fn compatible(local: DtlsSetup, peer: DtlsSetup) -> bool {
    matches!(
        (local, peer),
        (DtlsSetup::Active, DtlsSetup::ActPass | DtlsSetup::Passive)
            | (DtlsSetup::Passive, DtlsSetup::ActPass | DtlsSetup::Active)
    )
}

pub(super) fn establish(
    local: &SessionSecurity,
    peer: &SessionSecurity,
) -> Result<SessionSecurity, SipError> {
    match (local, peer) {
        (SessionSecurity::Rtp, SessionSecurity::Rtp) => Ok(SessionSecurity::Rtp),
        (SessionSecurity::WebRtc(local), SessionSecurity::WebRtc(peer)) => {
            let role = match local.setup() {
                DtlsSetup::ActPass => match peer.setup() {
                    DtlsSetup::Active => "passive",
                    DtlsSetup::Passive => "active",
                    DtlsSetup::ActPass => return Err(SipError::Unsupported),
                },
                DtlsSetup::Active | DtlsSetup::Passive => {
                    if !compatible(local.setup(), peer.setup()) {
                        return Err(SipError::Unsupported);
                    }
                    match local.setup() {
                        DtlsSetup::Active => "active",
                        DtlsSetup::Passive => "passive",
                        DtlsSetup::ActPass => return Err(SipError::Unsupported),
                    }
                }
            };
            let mut source = String::with_capacity(local.as_str().len());
            for line in local.as_str().lines() {
                if line.starts_with("a=tls-id:") && peer.tls_id().is_none() {
                    continue;
                }
                if line.starts_with("a=setup:") {
                    source.push_str(&format!("a=setup:{role}\r\n"));
                } else {
                    source.push_str(line);
                    source.push_str("\r\n");
                }
            }
            Ok(SessionSecurity::WebRtc(WebRtcDescription::parse(&source)?))
        }
        (SessionSecurity::Rtp, SessionSecurity::WebRtc(_))
        | (SessionSecurity::WebRtc(_), SessionSecurity::Rtp) => Err(SipError::Unsupported),
    }
}

pub(super) fn version(local: &SessionSecurity, fallback: u64) -> Result<u64, SipError> {
    match local {
        SessionSecurity::Rtp => Ok(fallback),
        SessionSecurity::WebRtc(description) => {
            origin(description.as_str()).map(|(_, version)| version)
        }
    }
}

pub(super) fn update(
    previous: &[u8],
    offered: &[u8],
    session: &Session,
    local: &SessionSecurity,
) -> Result<Session, SipError> {
    let previous = std::str::from_utf8(previous)?;
    let offered = std::str::from_utf8(offered)?;
    let updated = Session::parse(offered, &[session.codec])?;
    let webrtc = match (local, &updated.security) {
        (SessionSecurity::Rtp, SessionSecurity::Rtp) => false,
        (SessionSecurity::WebRtc(local), SessionSecurity::WebRtc(peer)) => {
            if !compatible(local.setup(), peer.setup()) {
                return Err(SipError::Unsupported);
            }
            true
        }
        (SessionSecurity::Rtp, SessionSecurity::WebRtc(_))
        | (SessionSecurity::WebRtc(_), SessionSecurity::Rtp) => return Err(SipError::Unsupported),
    };
    let (old_identity, old_version) = origin(previous)?;
    let (identity, version) = origin(offered)?;
    if identity != old_identity
        || (version == old_version && previous != offered)
        || (version != old_version && old_version.checked_add(1) != Some(version))
        || !previous
            .lines()
            .filter(|line| unchanged(line, webrtc))
            .eq(offered.lines().filter(|line| unchanged(line, webrtc)))
        || updated.remote != session.remote
        || updated.codec != session.codec
        || updated.payload_type != session.payload_type
        || updated.telephone != session.telephone
    {
        return Err(SipError::Unsupported);
    }
    Ok(updated)
}

pub(super) fn answer(
    session: &Session,
    local: &SessionSecurity,
    address: SocketAddr,
    session_id: u64,
    version: u64,
) -> Result<String, SipError> {
    let description = match local {
        SessionSecurity::Rtp => return session.answer_with_version(address, session_id, version),
        SessionSecurity::WebRtc(description) => description,
    };
    let ([user, id, network, family, host], _) = origin(description.as_str())?;
    let mut source = String::with_capacity(description.as_str().len());
    for line in description.as_str().lines() {
        if line.starts_with("o=") {
            source.push_str(&format!(
                "o={user} {id} {version} {network} {family} {host}\r\n"
            ));
        } else if !matches!(
            line,
            "a=sendrecv" | "a=sendonly" | "a=recvonly" | "a=inactive"
        ) {
            source.push_str(line);
            source.push_str("\r\n");
        }
    }
    source.push_str(match session.direction {
        Direction::SendRecv => "a=sendrecv\r\n",
        Direction::SendOnly => "a=recvonly\r\n",
        Direction::RecvOnly => "a=sendonly\r\n",
        Direction::Inactive => "a=inactive\r\n",
    });
    WebRtcDescription::parse(&source)?;
    Ok(source)
}
