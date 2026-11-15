use crate::{
    media::MediaPort,
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
pub(super) enum PendingMedia {
    Rtp(SocketPair),
    WebRtc(PendingWebRtc),
}

pub(super) enum PendingWebRtc {
    Offer(WebRtcOffer),
    Session(WebRtcSession),
}

impl PendingMedia {
    pub(super) async fn attach(
        self,
        media: &MediaPort,
        key: CallKey,
        session: &Session,
    ) -> Result<(), MediaError> {
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
                media
                    .attach(
                        key,
                        sockets,
                        RtpSession {
                            remote: session.remote,
                            format,
                            payload_type: session.payload_type,
                            telephone,
                        },
                        direction,
                    )
                    .await
            }
            (Self::WebRtc(pending), SessionSecurity::WebRtc(description)) => {
                let rtc = match pending {
                    PendingWebRtc::Offer(offer) => offer.accept_answer(description.as_str())?,
                    PendingWebRtc::Session(session) => session,
                };
                media.attach_webrtc(key, rtc, direction).await
            }
            (Self::Rtp(_), SessionSecurity::WebRtc(_))
            | (Self::WebRtc(_), SessionSecurity::Rtp) => Err(MediaError::Unsupported),
        }
    }
}
