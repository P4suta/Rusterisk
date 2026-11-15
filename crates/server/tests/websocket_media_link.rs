use rusterisk_api::{ChannelId, MediaControlFormat, MediaDirection, MediaEvent, MediaFormat};
use rusterisk_core::PeerMediaDirection;
use rusterisk_media::{Frame, MediaError};
use rusterisk_server::{
    media_link::Channels,
    media_websocket::{MediaLinkOptions, MediaSocket, SocketMessage, WebSocketMediaLink},
};
use std::time::Duration;
use tokio::sync::{mpsc, watch};

struct Socket {
    input: mpsc::Receiver<SocketMessage>,
    output: mpsc::Sender<SocketMessage>,
}
impl MediaSocket for Socket {
    async fn flush(&mut self) -> Result<(), MediaError> {
        Ok(())
    }

    async fn receive(&mut self) -> Result<Option<SocketMessage>, MediaError> {
        Ok(self.input.recv().await)
    }
    async fn send(&mut self, message: SocketMessage) -> Result<(), MediaError> {
        self.output
            .try_send(message)
            .map_err(|_| MediaError::Capacity)
    }
}

fn failure(error: impl std::fmt::Display) -> MediaError {
    MediaError::Transport(error.to_string())
}

#[tokio::test]
async fn websocket_links_keep_control_messages_and_audio_independent() -> Result<(), MediaError> {
    let (peer, input) = mpsc::channel(32);
    let (output, mut messages) = mpsc::channel(32);
    let (incoming, mut frames) = mpsc::channel(3);
    let (outgoing, sent) = mpsc::channel(3);
    let (digits, _digits) = mpsc::channel(16);
    let (_commands, commands) = mpsc::channel(8);
    let (completed, _completed) = mpsc::channel(8);
    let (stop, stopped) = watch::channel(false);
    let (_direction, directed) = watch::channel(PeerMediaDirection::SendReceive);
    let (answer, mut answers) = mpsc::channel(8);
    let options = MediaLinkOptions {
        connection_id: ChannelId::try_from("connection".to_owned()).map_err(failure)?,
        channel_id: ChannelId::try_from("channel".to_owned()).map_err(failure)?,
        channel: "WebSocket/INCOMING/channel".into(),
        format: MediaFormat::Pcmu,
        control_format: MediaControlFormat::Json,
        direction: MediaDirection::Both,
        auto_answer: true,
        channel_variables: Default::default(),
    };
    let link = WebSocketMediaLink::new(
        Socket { input, output },
        Channels {
            incoming,
            digits,
            outgoing: sent,
            commands,
            completed,
            stop: stopped,
            direction: directed,
        },
        options,
        answer,
    )?;
    let task = tokio::spawn(link.run());
    let result = async {
        let Some(SocketMessage::Text(start)) = messages.recv().await else { return Err(MediaError::InvalidPacket); };
        assert!(matches!(serde_json::from_str::<MediaEvent>(&start).map_err(failure)?, MediaEvent::MediaStart { .. }));
        assert_eq!(answers.recv().await, Some(()));
        outgoing.send(Frame::new(&[0; 160], 8000)?).await.map_err(failure)?;
        assert_eq!(messages.recv().await, Some(SocketMessage::Binary(axum::body::Bytes::from_static(&[0xff; 160]))));
        peer.send(SocketMessage::Text(r#"{"command":"START_MEDIA_BUFFERING"}"#.into())).await.map_err(failure)?;
        peer.send(SocketMessage::Binary(axum::body::Bytes::from_static(&[0xff; 80]))).await.map_err(failure)?;
        peer.send(SocketMessage::Text(r#"{"command":"STOP_MEDIA_BUFFERING","correlation_id":"transfer"}"#.into())).await.map_err(failure)?;
        assert_eq!(tokio::time::timeout(Duration::from_secs(1), frames.recv()).await.map_err(failure)?, Some(Frame::new(&[0; 160], 8000)?));
        let Some(SocketMessage::Text(finished)) = tokio::time::timeout(Duration::from_secs(1), messages.recv()).await.map_err(failure)? else { return Err(MediaError::InvalidPacket); };
        assert!(matches!(serde_json::from_str::<MediaEvent>(&finished).map_err(failure)?, MediaEvent::MediaBufferingCompleted { correlation_id, .. } if correlation_id == "transfer"));
        peer.send(SocketMessage::Text(r#"{"command":"HANGUP"}"#.into())).await.map_err(failure)?;
        assert_eq!(messages.recv().await, Some(SocketMessage::Close));
        Ok(())
    }.await;
    stop.send_replace(true);
    task.await.map_err(failure)??;
    result
}
