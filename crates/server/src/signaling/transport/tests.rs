use super::{
    connection, failure, listen, Config, ConnectionKey, ConnectionState, ControlError, Datagram,
    Input, ListenerInput, Network, Reserved, StreamEvent, StreamPacket, StreamRole,
};
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::{mpsc, watch},
};
mod deadlines;
mod deferred;
mod preparations;
mod registration_retirement;
mod routes;
mod upgrades;

const OPTIONS: &[u8] = b"OPTIONS sip:localhost SIP/2.0\r\nContent-Length: 0\r\n\r\n";

async fn network() -> Result<Network, ControlError> {
    let mut config = Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=test\n",
        "[stream]\ntype=transport\nprotocol=tcp\nbind=127.0.0.1:0\n",
        "",
        "",
    )
    .map_err(failure)?;
    Network::bind(&mut config).await
}

#[tokio::test]
async fn capacity_refuses_the_next_socket_and_shutdown_releases_all_owned_streams(
) -> Result<(), ControlError> {
    let mut network = network().await?;
    let address = network.local_addr(0)?;
    let mut peers = Vec::with_capacity(64);
    for _ in 0..64 {
        let mut peer = TcpStream::connect(address).await.map_err(failure)?;
        peer.write_all(OPTIONS).await.map_err(failure)?;
        let Input::Packet(packet) = network.next().await? else {
            return Err(ControlError::Capacity);
        };
        assert_eq!(packet.bytes, OPTIONS);
        peers.push(peer);
    }
    let mut extra = TcpStream::connect(address).await.map_err(failure)?;
    let result = tokio::time::timeout(Duration::from_millis(300), async {
        let mut byte = [0; 1];
        tokio::select! {
            read = extra.read(&mut byte) => { assert_eq!(read.map_err(failure)?, 0); Ok(()) },
            received = network.next() => { received?; Err(ControlError::Invalid("Capacity changed an active stream")) },
        }
    }).await.map_err(failure)?;
    let cleanup = network.close().await;
    drop(peers);
    drop(network);
    let listener = tokio::net::TcpListener::bind(address)
        .await
        .map_err(failure)?;
    assert_eq!(listener.local_addr().map_err(failure)?, address);
    cleanup?;
    result
}

#[tokio::test]
async fn output_saturation_closes_only_the_owning_connection_generation() -> Result<(), ControlError>
{
    let mut network = network().await?;
    let address = network.local_addr(0)?;
    let mut peer = TcpStream::connect(address).await.map_err(failure)?;
    let source = peer.local_addr().map_err(failure)?;
    peer.write_all(OPTIONS).await.map_err(failure)?;
    assert!(matches!(network.next().await?, Input::Packet(_)));
    let key = network
        .legacy_connection(0, source)?
        .ok_or(ControlError::Capacity)?;
    for _ in 0..9 {
        network
            .send(0, Datagram::new(source, OPTIONS.to_vec()))
            .await?;
    }
    assert!(matches!(
        network.ports.get(&key).map(|port| &port.state),
        Some(ConnectionState::Closing(_))
    ));
    let Input::Closed {
        key: closed,
        result,
    } = tokio::time::timeout(Duration::from_secs(1), network.next())
        .await
        .map_err(failure)??
    else {
        return Err(ControlError::Capacity);
    };
    assert_eq!(closed, key);
    assert!(result.is_err());
    assert!(network.ports.is_empty());
    network.close().await
}

#[tokio::test]
async fn delayed_packets_and_completions_cannot_close_a_replacement_generation(
) -> Result<(), ControlError> {
    let mut network = network().await?;
    let peer = "127.0.0.1:5061".parse().map_err(failure)?;
    let Reserved { key: previous, .. } = network.reserve(0, peer)?;
    network.ports.remove(&previous);
    let Reserved {
        key: current,
        packets,
        stop,
    } = network.reserve(0, peer)?;
    assert!(current.generation > previous.generation);
    network
        .sender
        .send(StreamPacket {
            key: previous,
            item: StreamEvent::Message(b"old".to_vec()),
        })
        .await
        .map_err(failure)?;
    network
        .sender
        .send(StreamPacket {
            key: current,
            item: StreamEvent::Message(b"current".to_vec()),
        })
        .await
        .map_err(failure)?;
    network.jobs.spawn(async move { Ok(()) });
    let Input::Packet(packet) = network.next().await? else {
        return Err(ControlError::Capacity);
    };
    assert_eq!(packet.bytes, b"current");
    assert!(
        tokio::time::timeout(Duration::from_millis(20), network.next())
            .await
            .is_err()
    );
    assert_eq!(
        network
            .ports
            .get(&current)
            .ok_or(ControlError::Capacity)?
            .key,
        current
    );
    network.close().await?;
    drop((packets, stop));
    Ok(())
}

#[tokio::test]
async fn cancellation_drops_a_partial_frame_and_releases_the_stream() -> Result<(), ControlError> {
    let (mut peer, stream) = tokio::io::duplex(1024);
    let key = ConnectionKey {
        listener: 0,
        peer: "127.0.0.1:5061".parse().map_err(failure)?,
        generation: 1,
    };
    let (packets, input) = mpsc::channel(8);
    let (sender, mut received) = mpsc::channel(8);
    let (stop, stopped) = watch::channel(false);
    let task = tokio::spawn(connection(
        stream,
        key,
        input,
        sender,
        stopped,
        StreamRole::Accepted,
    ));
    peer.write_all(b"OPTIONS sip:localhost SIP/2.0\r\nContent-Len")
        .await
        .map_err(failure)?;
    tokio::task::yield_now().await;
    stop.send_replace(true);
    tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .map_err(failure)?
        .map_err(failure)??;
    let mut byte = [0; 1];
    assert_eq!(peer.read(&mut byte).await.map_err(failure)?, 0);
    assert!(matches!(
        received.recv().await.map(|packet| packet.item),
        Some(StreamEvent::Finished(Ok(())))
    ));
    assert!(received.recv().await.is_none());
    drop(packets);
    Ok(())
}

#[tokio::test]
async fn a_full_event_queue_cannot_hold_shutdown_open() -> Result<(), ControlError> {
    let mut network = network().await?;
    let key = ConnectionKey {
        listener: 0,
        peer: "127.0.0.1:5061".parse().map_err(failure)?,
        generation: 0,
    };
    for _ in 0..128 {
        network
            .sender
            .try_send(StreamPacket {
                key,
                item: StreamEvent::Message(OPTIONS.to_vec()),
            })
            .map_err(failure)?;
    }
    let mut peer = TcpStream::connect(network.local_addr(0)?)
        .await
        .map_err(failure)?;
    let ListenerInput::Accepted {
        listener,
        peer: source,
        stream,
    } = listen(&network.listeners).await?
    else {
        return Err(ControlError::Capacity);
    };
    network.accepted(listener, source, stream)?;
    peer.write_all(OPTIONS).await.map_err(failure)?;
    tokio::task::yield_now().await;
    tokio::time::timeout(Duration::from_secs(1), network.close())
        .await
        .map_err(failure)??;
    assert!(network.jobs.is_empty());
    assert!(network.ports.is_empty());
    Ok(())
}

#[tokio::test]
async fn keepalives_cannot_extend_the_first_message_deadline() -> Result<(), ControlError> {
    let mut network = network().await?;
    let mut peer = TcpStream::connect(network.local_addr(0)?)
        .await
        .map_err(failure)?;
    peer.write_all(b"\r\n\r\n").await.map_err(failure)?;
    let mut pong = [0; 2];
    let (input, pong) = tokio::time::timeout(Duration::from_secs(6), async {
        tokio::join!(network.next(), peer.read_exact(&mut pong))
    })
    .await
    .map_err(failure)?;
    pong.map_err(failure)?;
    assert!(matches!(input?, Input::Closed { result: Err(_), .. }));
    network.close().await
}

#[tokio::test]
async fn a_final_frame_and_its_response_precede_half_closed_connection_cleanup(
) -> Result<(), ControlError> {
    for _ in 0..16 {
        let mut network = network().await?;
        let mut peer = TcpStream::connect(network.local_addr(0)?)
            .await
            .map_err(failure)?;
        let source = peer.local_addr().map_err(failure)?;
        peer.write_all(OPTIONS).await.map_err(failure)?;
        peer.shutdown().await.map_err(failure)?;
        let Input::Packet(packet) = network.next().await? else {
            return Err(ControlError::Invalid(
                "Connection closed before its final message",
            ));
        };
        assert_eq!(packet.bytes, OPTIONS);
        network
            .send(0, Datagram::new(source, b"response".to_vec()))
            .await?;
        let Input::Closed { result, .. } = network.next().await? else {
            return Err(ControlError::Invalid("Expected connection cleanup"));
        };
        result?;
        let mut response = Vec::new();
        tokio::time::timeout(Duration::from_secs(1), peer.read_to_end(&mut response))
            .await
            .map_err(failure)?
            .map_err(failure)?;
        assert_eq!(response, b"response");
        network.close().await?;
    }
    Ok(())
}
