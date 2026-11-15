use super::{emit, failure, finished, ConnectionKey, ControlError, StreamEvent, StreamPacket};
use axum::extract::ws::{Message, WebSocket};
use futures_util::SinkExt;
use rusterisk_sip::validate_websocket_message;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot, watch};

async fn send(socket: &mut WebSocket, message: Message) -> Result<(), ControlError> {
    tokio::time::timeout(Duration::from_millis(100), socket.send(message))
        .await
        .map_err(failure)?
        .map_err(failure)
}

async fn packet(socket: &mut WebSocket, bytes: Vec<u8>) -> Result<(), ControlError> {
    let message = match String::from_utf8(bytes) {
        Ok(text) => Message::Text(text.into()),
        Err(error) => Message::Binary(error.into_bytes().into()),
    };
    send(socket, message).await
}

async fn drain(
    socket: &mut WebSocket,
    packets: &mut mpsc::Receiver<Vec<u8>>,
    stop: &mut watch::Receiver<bool>,
) -> Result<(), ControlError> {
    let mut watching = true;
    loop {
        if *stop.borrow() {
            return Ok(());
        }
        tokio::select! {
            changed = stop.changed(), if watching => match changed { Ok(()) => { if *stop.borrow() { return Ok(()); } }, Err(_) => watching = false },
            bytes = packets.recv() => match bytes { Some(bytes) => packet(socket, bytes).await?, None => return Ok(()) },
        }
    }
}

pub(super) async fn connection(
    received: oneshot::Receiver<WebSocket>,
    key: ConnectionKey,
    mut packets: mpsc::Receiver<Vec<u8>>,
    sender: mpsc::Sender<StreamPacket>,
    mut stop: watch::Receiver<bool>,
) -> Result<(), ControlError> {
    let accepted = tokio::select! {
        changed = stop.changed() => match changed { Ok(()) | Err(_) => return finished(&sender, key, Ok(()), &stop).await },
        accepted = tokio::time::timeout(Duration::from_secs(5), received) => accepted.map_err(failure).and_then(|result| result.map_err(failure)),
    };
    let mut socket = match accepted {
        Ok(socket) => socket,
        Err(error) => return finished(&sender, key, Err(error), &stop).await,
    };
    let mut peer_closed = false;
    let result = async {
        let first_deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let mut first = true;
        let mut received = tokio::time::Instant::now();
        let period = Duration::from_secs(20);
        let mut heartbeat = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            if *stop.borrow() { break; }
            tokio::select! {
                changed = stop.changed() => if changed.is_err() || *stop.borrow() { break; },
                () = tokio::time::sleep_until(first_deadline), if first => return Err(ControlError::Invalid("Initial signaling message timed out")),
                bytes = packets.recv() => match bytes { Some(bytes) => packet(&mut socket, bytes).await?, None => break },
                _instant = heartbeat.tick() => {
                    if received.elapsed() > Duration::from_secs(60) { return Err(ControlError::Invalid("Signaling heartbeat timed out")); }
                    send(&mut socket, Message::Ping(axum::body::Bytes::new())).await?;
                },
                message = socket.recv() => {
                    received = tokio::time::Instant::now();
                    let bytes = match message {
                        Some(Ok(Message::Text(text))) => text.as_bytes().to_vec(),
                        Some(Ok(Message::Binary(bytes))) => bytes.to_vec(),
                        Some(Ok(Message::Ping(bytes))) => { send(&mut socket, Message::Pong(bytes)).await?; continue; },
                        Some(Ok(Message::Pong(_))) => continue,
                        Some(Ok(Message::Close(frame))) => { peer_closed = true; send(&mut socket, Message::Close(frame)).await?; break; },
                        Some(Err(error)) => { peer_closed = true; return Err(failure(error)); },
                        None => { peer_closed = true; break; },
                    };
                    if bytes == b"\r\n\r\n" { send(&mut socket, Message::Text("\r\n".into())).await?; continue; }
                    if bytes == b"\r\n" { continue; }
                    validate_websocket_message(&bytes).map_err(failure)?;
                    first = false;
                    if !emit(&sender, StreamPacket { key, item: StreamEvent::Message(bytes) }, &mut stop).await? { break; }
                },
            }
        }
        Ok(())
    }.await;
    finished(&sender, key, result, &stop).await?;
    if !peer_closed {
        let result = drain(&mut socket, &mut packets, &mut stop).await;
        let cleanup = tokio::time::timeout(Duration::from_millis(100), socket.close())
            .await
            .map_err(failure)
            .and_then(|result| result.map_err(failure));
        match (result, cleanup) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
            (Err(primary), Err(cleanup)) => Err(ControlError::Adapter(format!(
                "{primary}; WebSocket cleanup failed: {cleanup}"
            ))),
        }
    } else {
        Ok(())
    }
}
