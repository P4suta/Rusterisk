use super::failure;
use rusterisk_server::{rtp::SocketPair, ControlError};
use std::net::{IpAddr, SocketAddr};
use tokio::net::UdpSocket;

pub(super) struct Peer {
    pub(super) signal: UdpSocket,
    pub(super) audio: SocketPair,
    advertised: IpAddr,
}

impl Peer {
    pub(super) async fn bind(bind: IpAddr, advertised: IpAddr) -> Result<Self, ControlError> {
        Ok(Self {
            signal: UdpSocket::bind((bind, 0)).await.map_err(failure)?,
            audio: SocketPair::bind(bind, 0, 0).map_err(failure)?,
            advertised,
        })
    }

    pub(super) fn contact(&self) -> Result<SocketAddr, ControlError> {
        Ok(SocketAddr::new(
            self.advertised,
            self.signal.local_addr().map_err(failure)?.port(),
        ))
    }

    pub(super) fn media(&self) -> Result<SocketAddr, ControlError> {
        Ok(SocketAddr::new(
            self.advertised,
            self.audio.local_addr().map_err(failure)?.port(),
        ))
    }

    pub(super) fn addresses(&self) -> Result<[SocketAddr; 3], ControlError> {
        Ok([
            self.signal.local_addr().map_err(failure)?,
            self.audio.local_addr().map_err(failure)?,
            self.audio.control_addr().map_err(failure)?,
        ])
    }
}
