use super::{failure, Signaling};
use crate::{
    rtp::{RtpSession, SocketPair},
    CallKey, ControlError,
};
use rusterisk_api::{ExternalRtpTarget, StaticRtpCodec};
use rusterisk_core::PeerMediaDirection;
use rusterisk_media::{AudioFormat, MediaError};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

impl Signaling {
    pub(super) async fn connect_external_rtp(
        &mut self,
        key: CallKey,
        target: ExternalRtpTarget,
    ) -> Result<SocketAddr, ControlError> {
        if self.calls.len() + self.external_media.len() >= 64 {
            return Err(ControlError::Capacity);
        }
        let ip = match target.remote().ip() {
            IpAddr::V4(_) => IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            IpAddr::V6(_) => IpAddr::V6(Ipv6Addr::UNSPECIFIED),
        };
        let route = std::net::UdpSocket::bind(SocketAddr::new(ip, 0)).map_err(failure)?;
        route.connect(target.remote()).map_err(failure)?;
        let ip = route.local_addr().map_err(failure)?.ip();
        drop(route);
        let sockets =
            SocketPair::bind(ip, self.config.rtp.start, self.config.rtp.end).map_err(failure)?;
        let local = sockets.local_addr().map_err(failure)?;
        let format = match target.codec() {
            StaticRtpCodec::Pcmu => AudioFormat::Pcmu,
            StaticRtpCodec::Pcma => AudioFormat::Pcma,
        };
        self.media
            .attach(
                key.clone(),
                sockets,
                RtpSession {
                    remote: target.remote(),
                    format,
                    payload_type: target.codec().payload_type(),
                    telephone: None,
                },
                PeerMediaDirection::SendReceive,
            )
            .await
            .map_err(|error| match error {
                MediaError::Capacity => ControlError::Capacity,
                error @ (MediaError::InvalidFrame
                | MediaError::InvalidPacket
                | MediaError::InvalidRoute
                | MediaError::UnknownStream
                | MediaError::Unsupported
                | MediaError::Codec(_)
                | MediaError::Transport(_)
                | MediaError::Control(_)) => failure(error),
            })?;
        self.external_media.insert(key);
        Ok(local)
    }
}
