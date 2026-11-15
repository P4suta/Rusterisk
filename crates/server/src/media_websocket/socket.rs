use axum::{
    body::Bytes,
    extract::ws::{Message, WebSocket},
};
use rusterisk_media::MediaError;
use std::future::Future;

#[derive(Debug, PartialEq, Eq)]
pub enum SocketMessage {
    Text(String),
    Binary(Bytes),
    Ping(Bytes),
    Pong(Bytes),
    Close,
}

pub trait MediaSocket: Send {
    fn receive(&mut self)
        -> impl Future<Output = Result<Option<SocketMessage>, MediaError>> + Send;
    fn send(
        &mut self,
        message: SocketMessage,
    ) -> impl Future<Output = Result<(), MediaError>> + Send;
}

fn failure(error: impl std::fmt::Display) -> MediaError {
    MediaError::Transport(error.to_string())
}

impl MediaSocket for WebSocket {
    async fn receive(&mut self) -> Result<Option<SocketMessage>, MediaError> {
        self.recv()
            .await
            .transpose()
            .map_err(failure)
            .map(|message| {
                message.map(|message| match message {
                    Message::Text(text) => SocketMessage::Text(text.to_string()),
                    Message::Binary(bytes) => SocketMessage::Binary(bytes),
                    Message::Ping(bytes) => SocketMessage::Ping(bytes),
                    Message::Pong(bytes) => SocketMessage::Pong(bytes),
                    Message::Close(_) => SocketMessage::Close,
                })
            })
    }

    async fn send(&mut self, message: SocketMessage) -> Result<(), MediaError> {
        let message = match message {
            SocketMessage::Text(text) => Message::Text(text.into()),
            SocketMessage::Binary(bytes) => Message::Binary(bytes),
            SocketMessage::Ping(bytes) => Message::Ping(bytes),
            SocketMessage::Pong(bytes) => Message::Pong(bytes),
            SocketMessage::Close => Message::Close(None),
        };
        WebSocket::send(self, message).await.map_err(failure)
    }
}

type ClientSocket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

impl MediaSocket for ClientSocket {
    async fn receive(&mut self) -> Result<Option<SocketMessage>, MediaError> {
        let message = futures_util::StreamExt::next(self)
            .await
            .transpose()
            .map_err(failure)?;
        message
            .map(|message| {
                Ok(match message {
                    tokio_tungstenite::tungstenite::Message::Text(text) => {
                        SocketMessage::Text(text.to_string())
                    }
                    tokio_tungstenite::tungstenite::Message::Binary(bytes) => {
                        SocketMessage::Binary(bytes)
                    }
                    tokio_tungstenite::tungstenite::Message::Ping(bytes) => {
                        SocketMessage::Ping(bytes)
                    }
                    tokio_tungstenite::tungstenite::Message::Pong(bytes) => {
                        SocketMessage::Pong(bytes)
                    }
                    tokio_tungstenite::tungstenite::Message::Close(_) => SocketMessage::Close,
                    tokio_tungstenite::tungstenite::Message::Frame(_) => {
                        return Err(MediaError::InvalidPacket)
                    }
                })
            })
            .transpose()
    }

    async fn send(&mut self, message: SocketMessage) -> Result<(), MediaError> {
        use tokio_tungstenite::tungstenite::Message as ClientMessage;
        let message = match message {
            SocketMessage::Text(text) => ClientMessage::Text(text.into()),
            SocketMessage::Binary(bytes) => ClientMessage::Binary(bytes),
            SocketMessage::Ping(bytes) => ClientMessage::Ping(bytes),
            SocketMessage::Pong(bytes) => ClientMessage::Pong(bytes),
            SocketMessage::Close => ClientMessage::Close(None),
        };
        futures_util::SinkExt::send(self, message)
            .await
            .map_err(failure)
    }
}
