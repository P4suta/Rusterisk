use super::{network, ConnectionState, ControlError, Datagram};
use crate::signaling::transport::Network;
use rusterisk_config::Config;
use std::net::SocketAddr;
use tokio::net::UdpSocket;
use tokio::sync::mpsc::error::TryRecvError;

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}

fn packet(peer: SocketAddr, value: &[u8]) -> Datagram {
    Datagram::new(peer, value.to_vec())
}

#[tokio::test]
async fn captured_stream_routes_keep_their_connection_generation_across_many_packets(
) -> Result<(), ControlError> {
    let mut network = network().await?;
    let peer = "127.0.0.1:5061".parse().map_err(failure)?;
    let mut previous = network.reserve(0, peer)?;
    let route = network.capture(0, peer)?;
    assert_eq!(route.listener(), 0);
    assert_eq!(route.peer(), peer);
    for bytes in [b"initial".as_slice(), b"progress", b"final"] {
        assert!(network.send_on(route, packet(peer, bytes)).await?);
        assert_eq!(previous.packets.try_recv().map_err(failure)?, bytes);
    }
    network.ports.remove(&previous.key);
    let mut current = network.reserve(0, peer)?;
    assert!(!network.send_on(route, packet(peer, b"stale")).await?);
    assert!(matches!(
        current.packets.try_recv(),
        Err(TryRecvError::Empty)
    ));
    let route = network.capture(0, peer)?;
    assert!(network.send_on(route, packet(peer, b"current")).await?);
    assert_eq!(current.packets.try_recv().map_err(failure)?, b"current");
    network.close().await
}

#[tokio::test]
async fn missing_or_closing_route_captures_never_attach_to_replacement_connections(
) -> Result<(), ControlError> {
    for closing in [false, true] {
        let mut network = network().await?;
        let peer = "127.0.0.1:5061".parse().map_err(failure)?;
        let previous = if closing {
            let previous = network.reserve(0, peer)?;
            network
                .ports
                .get_mut(&previous.key)
                .ok_or(ControlError::Capacity)?
                .state = ConnectionState::Closing(ControlError::Capacity);
            Some(previous)
        } else {
            None
        };
        let route = network.capture(0, peer)?;
        assert_eq!(route.listener(), 0);
        assert_eq!(route.peer(), peer);
        if let Some(previous) = &previous {
            network.ports.remove(&previous.key);
        }
        drop(previous);
        let mut replacement = network.reserve(0, peer)?;
        assert!(!network.send_on(route, packet(peer, b"old")).await?);
        assert!(matches!(
            replacement.packets.try_recv(),
            Err(TryRecvError::Empty)
        ));
        network.close().await?;
    }
    Ok(())
}

#[tokio::test]
async fn captured_routes_reject_peer_changes_and_oversize_packets_before_delivery(
) -> Result<(), ControlError> {
    let mut network = network().await?;
    let peer = "127.0.0.1:5061".parse().map_err(failure)?;
    let mut connection = network.reserve(0, peer)?;
    let route = network.capture(0, peer)?;
    let other = "127.0.0.1:5062".parse().map_err(failure)?;
    assert!(matches!(
        network.send_on(route, packet(other, b"wrong peer")).await,
        Err(ControlError::Invalid(_))
    ));
    assert!(matches!(
        network
            .send_on(route, packet(peer, &[0; rusterisk_sip::MAX_MESSAGE + 1]))
            .await,
        Err(ControlError::Capacity)
    ));
    assert!(matches!(
        network.ports.get(&connection.key).map(|port| &port.state),
        Some(ConnectionState::Open)
    ));
    assert!(matches!(
        connection.packets.try_recv(),
        Err(TryRecvError::Empty)
    ));
    assert!(network.send_on(route, packet(peer, b"valid")).await?);
    assert_eq!(connection.packets.try_recv().map_err(failure)?, b"valid");
    network.close().await
}

#[tokio::test]
async fn captured_datagram_routes_preserve_the_listener_and_remote_address(
) -> Result<(), ControlError> {
    let mut config = Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=test\n",
        "[udp]\ntype=transport\nprotocol=udp\nbind=127.0.0.1:0\n",
        "",
        "",
    )
    .map_err(failure)?;
    let mut network = Network::bind(&mut config).await?;
    let listener = network.local_addr(0)?;
    let receiver = UdpSocket::bind("127.0.0.1:0").await.map_err(failure)?;
    let peer = receiver.local_addr().map_err(failure)?;
    let route = network.capture(0, peer)?;
    assert_eq!(route.listener(), 0);
    assert_eq!(route.peer(), peer);
    assert!(matches!(
        network.capture(1, peer),
        Err(ControlError::Invalid(_))
    ));
    for bytes in [b"initial".as_slice(), b"final"] {
        assert!(network.send_on(route, packet(peer, bytes)).await?);
        let mut buffer = [0; 32];
        let (count, source) = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            receiver.recv_from(&mut buffer),
        )
        .await
        .map_err(failure)?
        .map_err(failure)?;
        assert_eq!(&buffer[..count], bytes);
        assert_eq!(source, listener);
    }
    network.close().await
}
