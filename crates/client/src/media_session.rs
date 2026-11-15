use crate::{api, Client, Error};
use futures_util::{SinkExt, StreamExt};
use std::time::Duration;
use tokio_tungstenite::tungstenite::{
    client::IntoClientRequest, protocol::WebSocketConfig, Message,
};

pub use tokio_tungstenite::tungstenite::Bytes as MediaBytes;

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
const MAX_MESSAGE: usize = 65500;
const WRITE_DEADLINE: Duration = Duration::from_millis(100);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MediaMessage {
    Event(api::MediaEvent),
    Audio(MediaBytes),
}

pub struct MediaSession {
    socket: Option<Socket>,
    start: api::MediaEvent,
    channel: api::ChannelId,
    control_format: api::MediaControlFormat,
}

impl Client {
    pub async fn connect_media(
        &self,
        channel: &api::Channel,
        control_format: api::MediaControlFormat,
    ) -> Result<MediaSession, Error> {
        let connection = channel
            .channelvars
            .as_ref()
            .and_then(|variables| variables.get("MEDIA_WEBSOCKET_CONNECTION_ID"))
            .ok_or(Error::Invalid("channel has no media connection"))?;
        let connection = api::ChannelId::try_from(connection.clone())?;
        let mut url = self.base.clone();
        let scheme = match url.scheme() {
            "https" => "wss",
            "http" => "ws",
            _ => return Err(Error::Invalid("invalid media transport")),
        };
        url.set_scheme(scheme)
            .map_err(|()| Error::Invalid("invalid media transport"))?;
        url.set_path(&format!("/media/{}", connection.as_str()));
        let mut request = url.as_str().into_client_request()?;
        request.headers_mut().insert(
            "Sec-WebSocket-Protocol",
            reqwest::header::HeaderValue::from_static("media"),
        );
        let config = WebSocketConfig::default()
            .max_message_size(Some(MAX_MESSAGE))
            .max_frame_size(Some(MAX_MESSAGE))
            .write_buffer_size(0)
            .max_write_buffer_size(65536);
        let (mut socket, response) = tokio::time::timeout(
            Duration::from_secs(5),
            tokio_tungstenite::connect_async_with_config(request, Some(config), true),
        )
        .await
        .map_err(|_| Error::Timeout)??;
        if response.headers().get("Sec-WebSocket-Protocol")
            != Some(&reqwest::header::HeaderValue::from_static("media"))
        {
            return Err(Error::Invalid("invalid media subprotocol"));
        }
        let first = tokio::time::timeout(Duration::from_secs(5), socket.next())
            .await
            .map_err(|_| Error::Timeout)?
            .ok_or(Error::Closed)??;
        let Message::Text(text) = first else {
            return Err(Error::Invalid("expected media start event"));
        };
        let start =
            api::MediaEvent::parse(&text, control_format, &channel.id).map_err(event_error)?;
        if !matches!(&start, api::MediaEvent::MediaStart { connection_id, .. } if *connection_id == connection)
        {
            return Err(Error::Invalid("expected matching media start event"));
        }
        Ok(MediaSession {
            socket: Some(socket),
            start,
            channel: channel.id.clone(),
            control_format,
        })
    }
}

impl MediaSession {
    pub const fn start_event(&self) -> &api::MediaEvent {
        &self.start
    }

    pub async fn command(&mut self, command: api::MediaCommand) -> Result<(), Error> {
        let text = command
            .encode(self.control_format)
            .map_err(|error| match error {
                api::MediaCommandError::Capacity => Error::Capacity,
                api::MediaCommandError::Invalid => Error::Invalid("invalid media command"),
                api::MediaCommandError::UnsupportedFormat => {
                    Error::Invalid("media command requires JSON")
                }
                api::MediaCommandError::Json(error) => Error::Json(error),
            })?;
        self.send(Message::Text(text.into())).await
    }

    pub async fn send_audio(&mut self, audio: MediaBytes) -> Result<(), Error> {
        if audio.is_empty() {
            return Err(Error::Invalid("empty media audio"));
        }
        if audio.len() > MAX_MESSAGE {
            return Err(Error::Capacity);
        }
        self.send(Message::Binary(audio)).await
    }

    pub async fn next_message(&mut self) -> Result<Option<MediaMessage>, Error> {
        loop {
            let message = match self.socket.as_mut() {
                Some(socket) => socket.next().await,
                None => return Ok(None),
            };
            match message {
                Some(Ok(Message::Text(text))) => {
                    let event = api::MediaEvent::parse(&text, self.control_format, &self.channel)
                        .map_err(event_error);
                    if event.is_err() {
                        self.socket = None;
                    }
                    return event.map(|event| Some(MediaMessage::Event(event)));
                }
                Some(Ok(Message::Binary(bytes))) => return Ok(Some(MediaMessage::Audio(bytes))),
                Some(Ok(Message::Ping(_) | Message::Pong(_))) => self.flush().await?,
                Some(Ok(Message::Close(_))) => {
                    let result = self.flush().await;
                    self.socket = None;
                    result?;
                    return Ok(None);
                }
                Some(Ok(Message::Frame(_))) => {
                    self.socket = None;
                    return Err(Error::Invalid("unexpected media frame"));
                }
                Some(Err(error)) => {
                    self.socket = None;
                    return Err(error.into());
                }
                None => {
                    self.socket = None;
                    return Ok(None);
                }
            }
        }
    }

    pub async fn close(mut self) -> Result<(), Error> {
        let Some(mut socket) = self.socket.take() else {
            return Ok(());
        };
        tokio::time::timeout(WRITE_DEADLINE, socket.send(Message::Close(None)))
            .await
            .map_err(|_| Error::Timeout)??;
        tokio::time::timeout(Duration::from_secs(2), async {
            for _ in 0..128 {
                match socket.next().await {
                    Some(Ok(Message::Close(_))) => {
                        tokio::time::timeout(WRITE_DEADLINE, socket.flush())
                            .await
                            .map_err(|_| Error::Timeout)??;
                        return Ok(());
                    }
                    None => return Ok(()),
                    Some(Err(error)) => return Err(error.into()),
                    Some(Ok(_)) => {}
                }
            }
            Err(Error::Capacity)
        })
        .await
        .map_err(|_| Error::Timeout)?
    }

    async fn send(&mut self, message: Message) -> Result<(), Error> {
        let mut socket = self.socket.take().ok_or(Error::Closed)?;
        tokio::time::timeout(WRITE_DEADLINE, socket.send(message))
            .await
            .map_err(|_| Error::Timeout)??;
        self.socket = Some(socket);
        Ok(())
    }

    async fn flush(&mut self) -> Result<(), Error> {
        let mut socket = self.socket.take().ok_or(Error::Closed)?;
        tokio::time::timeout(WRITE_DEADLINE, socket.flush())
            .await
            .map_err(|_| Error::Timeout)??;
        self.socket = Some(socket);
        Ok(())
    }
}

fn event_error(error: api::MediaEventError) -> Error {
    match error {
        api::MediaEventError::Capacity => Error::Capacity,
        api::MediaEventError::Invalid => Error::Invalid("invalid media event"),
        api::MediaEventError::ForeignChannel => {
            Error::Invalid("media event belongs to another channel")
        }
    }
}
