use crate::signaling::calls;
use crate::{
    media::{AttachmentMode, AttachmentRelease, MediaPort},
    rtp::{RtpSession, SocketPair},
    webrtc::{WebRtcOffer, WebRtcSession},
    CallKey, ControlError,
};
use rusterisk_config::Codec;
use rusterisk_media::{AudioFormat, MediaError, TelephoneFormat};
use rusterisk_sip::{Engine, Session, SessionSecurity};
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

#[expect(
    clippy::large_enum_variant,
    reason = "docs/adr/own-webrtc-media-by-call-generation.md: Keep pending media inline and owned by the call generation."
)]
pub(super) enum MediaAdmission {
    Incoming(crate::call_profiles::NativeMediaProfile),
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
    Pending(AttachmentRelease),
    Missing,
}

impl super::Signaling {
    pub(super) fn queue_answered_attachment(
        &mut self,
        key: &CallKey,
        session: &Session,
    ) -> Result<AnsweredAttachment, MediaError> {
        // An attachment that passed owed orders could meet an older release for the same call.
        if !self.owed.is_empty() {
            return Err(MediaError::Capacity);
        }
        let serial = self.next_serial();
        let Some(call) = self.records.get_mut(calls::ACTIVE, key) else {
            return Ok(AnsweredAttachment::Missing);
        };
        match std::mem::replace(&mut call.media, MediaAdmission::Attached) {
            MediaAdmission::Early(_) => Ok(AnsweredAttachment::Existing),
            admission @ (MediaAdmission::Rtp(_) | MediaAdmission::WebRtc(_)) => admission
                .attach(
                    &self.media,
                    key.clone(),
                    session,
                    AttachmentMode::Answered,
                    serial,
                )
                .map(AnsweredAttachment::Pending),
            MediaAdmission::Incoming(_) | MediaAdmission::Attached => {
                Ok(AnsweredAttachment::Missing)
            }
        }
    }
}

impl MediaAdmission {
    // The media task reports the result as a notice that names the call.
    pub(super) fn attach(
        self,
        media: &MediaPort,
        key: CallKey,
        session: &Session,
        mode: AttachmentMode,
        serial: u64,
    ) -> Result<AttachmentRelease, MediaError> {
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
                media.start_attachment(
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
                    serial,
                )
            }
            (Self::WebRtc(pending), SessionSecurity::WebRtc(description)) => {
                let rtc = match pending {
                    PendingWebRtc::Offer(offer) => offer.accept_answer(description.as_str())?,
                    PendingWebRtc::Session(session) => session,
                };
                media.start_webrtc_attachment(key, rtc, direction, mode, serial)
            }
            (Self::Rtp(_), SessionSecurity::WebRtc(_))
            | (Self::WebRtc(_), SessionSecurity::Rtp)
            | (
                Self::Incoming(_) | Self::Early(_) | Self::Attached,
                SessionSecurity::Rtp | SessionSecurity::WebRtc(_),
            ) => Err(MediaError::Unsupported),
        }
    }
}
