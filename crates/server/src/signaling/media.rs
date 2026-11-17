use crate::{
    media::{AttachmentMode, AttachmentReceipt, MediaPort},
    rtp::{RtpSession, SocketPair},
    webrtc::{WebRtcOffer, WebRtcSession},
    CallKey, ControlError,
};
use rusterisk_config::Codec;
use rusterisk_media::{AudioFormat, MediaError, TelephoneFormat};
use rusterisk_sip::{Destination, Engine, Session, SessionSecurity, TargetHost};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

pub(super) fn local_ip(peer: SocketAddr) -> Result<IpAddr, ControlError> {
    let unspecified = if peer.is_ipv4() {
        Ipv4Addr::UNSPECIFIED.into()
    } else {
        Ipv6Addr::UNSPECIFIED.into()
    };
    let socket =
        std::net::UdpSocket::bind(SocketAddr::new(unspecified, 0)).map_err(super::failure)?;
    socket.connect(peer).map_err(super::failure)?;
    let address = socket
        .local_addr()
        .map_err(super::failure)?
        .ip()
        .to_canonical();
    if address.is_unspecified() || address.is_multicast() {
        return Err(ControlError::Invalid("No unicast media route found"));
    }
    Ok(address)
}

pub(super) fn destination_peer(
    engine: &Engine,
    endpoint: &str,
    resolved: Option<SocketAddr>,
    now: u64,
) -> Result<SocketAddr, ControlError> {
    if let Some(peer) = resolved {
        return Ok(peer);
    }
    match engine.destination(endpoint, now).map_err(super::failure)? {
        Destination::Registered(peer) => Ok(peer),
        Destination::Static(target) => {
            let TargetHost::Address(address) = target.host() else {
                return Err(ControlError::Unsupported);
            };
            let default = match target.protocol() {
                rusterisk_config::Protocol::Udp | rusterisk_config::Protocol::Tcp => 5060,
                rusterisk_config::Protocol::Tls => 5061,
                rusterisk_config::Protocol::Ws | rusterisk_config::Protocol::Wss => {
                    return Err(ControlError::Unsupported)
                }
            };
            Ok(SocketAddr::new(*address, target.port().unwrap_or(default)))
        }
    }
}

#[expect(
    clippy::large_enum_variant,
    reason = "docs/adr/own-webrtc-media-by-call-generation.md: Keep pending media inline and owned by the call generation."
)]
pub(super) enum MediaAdmission {
    Incoming,
    Rtp(SocketPair),
    WebRtc(PendingWebRtc),
    Early(PreparedAnswer),
    Attached,
}

pub(super) enum PreparedAnswer {
    Rtp(SocketAddr),
    WebRtc {
        local: SocketAddr,
        description: rusterisk_sip::WebRtcDescription,
    },
}

impl PreparedAnswer {
    pub(super) fn progress(
        &self,
        engine: &mut Engine,
        key: &CallKey,
        now: u64,
    ) -> Result<rusterisk_sip::Output, rusterisk_sip::SipError> {
        match self {
            Self::Rtp(local) => engine.progress(key.id.as_str(), key.generation, *local, now),
            Self::WebRtc { local, description } => {
                engine.progress_with_sdp(key.id.as_str(), key.generation, *local, description, now)
            }
        }
    }

    pub(super) fn answer(
        &self,
        engine: &mut Engine,
        key: &CallKey,
        now: u64,
    ) -> Result<rusterisk_sip::Output, rusterisk_sip::SipError> {
        match self {
            Self::Rtp(local) => engine.answer(key.id.as_str(), key.generation, *local, now),
            Self::WebRtc { local, description } => {
                engine.answer_with_sdp(key.id.as_str(), key.generation, *local, description, now)
            }
        }
    }
}

pub(super) enum PendingWebRtc {
    Offer(WebRtcOffer),
    Session(WebRtcSession),
}

pub(super) enum AnsweredAttachment {
    Existing,
    Pending(AttachmentReceipt),
    Missing,
}

impl super::Signaling {
    pub(super) fn queue_answered_attachment(
        &mut self,
        key: &CallKey,
        session: &Session,
    ) -> Result<AnsweredAttachment, MediaError> {
        let Some(call) = self.calls.get_mut(key) else {
            return Ok(AnsweredAttachment::Missing);
        };
        match std::mem::replace(&mut call.media, MediaAdmission::Attached) {
            MediaAdmission::Early(_) => Ok(AnsweredAttachment::Existing),
            admission @ (MediaAdmission::Rtp(_) | MediaAdmission::WebRtc(_)) => admission
                .attach(&self.media, key.clone(), session, AttachmentMode::Answered)
                .map(AnsweredAttachment::Pending),
            MediaAdmission::Incoming | MediaAdmission::Attached => Ok(AnsweredAttachment::Missing),
        }
    }
}

impl MediaAdmission {
    pub(super) fn attach(
        self,
        media: &MediaPort,
        key: CallKey,
        session: &Session,
        mode: AttachmentMode,
    ) -> Result<AttachmentReceipt, MediaError> {
        let direction = super::peer_direction(session.direction);
        match (self, &session.security) {
            (Self::Rtp(sockets), SessionSecurity::Rtp) => {
                let format = match session.codec {
                    Codec::Pcmu => AudioFormat::Pcmu,
                    Codec::Pcma => AudioFormat::Pcma,
                    Codec::Opus => AudioFormat::Opus,
                };
                let telephone = session
                    .telephone
                    .map(|format| {
                        TelephoneFormat::new(format.payload_type(), format.rate())?
                            .with_events(format.events())
                    })
                    .transpose()?;
                media.queue_attachment(
                    key,
                    sockets,
                    RtpSession {
                        remote: session.remote,
                        format,
                        payload_type: session.payload_type,
                        telephone,
                    },
                    direction,
                    mode,
                )
            }
            (Self::WebRtc(pending), SessionSecurity::WebRtc(description)) => {
                let rtc = match pending {
                    PendingWebRtc::Offer(offer) => offer.accept_answer(description.as_str())?,
                    PendingWebRtc::Session(session) => session,
                };
                media.queue_webrtc_attachment(key, rtc, direction, mode)
            }
            (Self::Rtp(_), SessionSecurity::WebRtc(_))
            | (Self::WebRtc(_), SessionSecurity::Rtp)
            | (
                Self::Incoming | Self::Early(_) | Self::Attached,
                SessionSecurity::Rtp | SessionSecurity::WebRtc(_),
            ) => Err(MediaError::Unsupported),
        }
    }
}
