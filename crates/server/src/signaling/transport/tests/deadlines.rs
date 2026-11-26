use super::{
    connection, failure, network, ConnectionKey, ControlError, Input, StreamEvent, StreamRole,
    OPTIONS,
};
use crate::signaling::cleanup;
use rusterisk_test_support::Wait;
use std::time::Duration;
use tokio::{
    io::AsyncWriteExt,
    net::TcpStream,
    sync::{mpsc, watch},
};

#[tokio::test(start_paused = true)]
async fn accepted_silence_keeps_the_five_second_first_message_deadline() -> Result<(), ControlError>
{
    let mut network = network().await?;
    let peer = TcpStream::connect(network.local_addr(0)?)
        .await
        .map_err(failure)?;
    let primary: Result<(), ControlError> = async {
        let start = tokio::time::Instant::now();
        let Input::Closed { result, .. } =
            rusterisk_test_support::virtual_window(Duration::from_secs(6), network.next())
                .await
                .map_err(failure)??
        else {
            return Err(ControlError::Invalid(
                "Silent accepted stream emitted a packet",
            ));
        };
        assert_eq!(start.elapsed(), Duration::from_secs(5));
        if !matches!(
            result,
            Err(ControlError::Invalid("Signaling frame timed out"))
        ) {
            return Err(ControlError::Invalid(
                "Silent accepted stream lost its frame deadline",
            ));
        }
        Ok(())
    }
    .await;
    drop(peer);
    cleanup::combine(primary, network.close().await)
}

async fn partial_deadline(role: StreamRole, complete_first: bool) -> Result<(), ControlError> {
    let (socket, mut peer) = tokio::io::duplex(1);
    let key = ConnectionKey {
        listener: 0,
        peer: "127.0.0.1:5060".parse().map_err(failure)?,
        generation: 1,
    };
    let (packets, input) = mpsc::channel(8);
    let (sender, mut received) = mpsc::channel(8);
    let (stop, stopped) = watch::channel(false);
    let mut task = tokio::spawn(connection(socket, key, input, sender, stopped, role));
    let primary: Result<(), ControlError> = async {
        if complete_first {
            peer.write_all(OPTIONS).await.map_err(failure)?;
            let complete = Wait::Ready
                .bounded(received.recv())
                .await
                .map_err(failure)?
                .ok_or(ControlError::Capacity)?;
            if complete.key != key
                || !matches!(complete.item, StreamEvent::Message(bytes) if bytes == OPTIONS)
            {
                return Err(ControlError::Invalid(
                    "Complete frame changed before deadline control",
                ));
            }
        }
        let start = tokio::time::Instant::now();
        peer.write_all(b"OPTIONS sip:localhost SIP/2.0\r\nContent-Length:")
            .await
            .map_err(failure)?;
        tokio::time::advance(Duration::from_secs(3)).await;
        peer.write_all(b" 0").await.map_err(failure)?;
        let expired = Wait::Ready
            .bounded(received.recv())
            .await
            .map_err(failure)?
            .ok_or(ControlError::Capacity)?;
        assert_eq!(start.elapsed(), Duration::from_secs(5));
        if expired.key != key
            || !matches!(
                expired.item,
                StreamEvent::Finished(Err(ControlError::Invalid("Signaling frame timed out")))
            )
        {
            return Err(ControlError::Invalid(
                "Partial frame extended its assembly deadline",
            ));
        }
        Ok(())
    }
    .await;
    stop.send_replace(true);
    drop((peer, packets));
    let cleanup = match Wait::Ready.bounded(&mut task).await {
        Ok(result) => result.map_err(failure).and_then(|result| result),
        Err(timeout) => {
            task.abort();
            let joined = match task.await {
                Err(error) if error.is_cancelled() => Ok(()),
                Err(error) => Err(failure(error)),
                Ok(result) => result,
            };
            cleanup::combine(Err(failure(timeout)), joined)
        }
    };
    cleanup::combine(primary, cleanup)
}

#[tokio::test(start_paused = true)]
async fn accepted_partial_frames_keep_their_original_deadline() -> Result<(), ControlError> {
    partial_deadline(StreamRole::Accepted, false).await
}

#[tokio::test(start_paused = true)]
async fn initiated_partial_frames_keep_their_original_deadline() -> Result<(), ControlError> {
    partial_deadline(StreamRole::Initiated, false).await
}

#[tokio::test(start_paused = true)]
async fn accepted_partial_frames_after_complete_messages_keep_their_original_deadline(
) -> Result<(), ControlError> {
    partial_deadline(StreamRole::Accepted, true).await
}

#[tokio::test(start_paused = true)]
async fn initiated_partial_frames_after_complete_messages_keep_their_original_deadline(
) -> Result<(), ControlError> {
    partial_deadline(StreamRole::Initiated, true).await
}
