use crate::{api, Client, Error};
use futures_util::{SinkExt, StreamExt};
use std::time::Duration;
use tokio::sync::{broadcast, watch};
use tokio_tungstenite::tungstenite::{
    client::IntoClientRequest, protocol::WebSocketConfig, Message,
};

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

pub struct Session {
    events: broadcast::Receiver<api::Event>,
    stop: watch::Sender<bool>,
    task: Option<tokio::task::JoinHandle<Result<(), Error>>>,
}

impl Session {
    pub async fn next_event(&mut self) -> Result<api::Event, Error> {
        self.events.recv().await.map_err(|error| match error {
            broadcast::error::RecvError::Closed => Error::Closed,
            broadcast::error::RecvError::Lagged(count) => Error::EventLagged(count),
        })
    }

    pub async fn shutdown(mut self) -> Result<(), Error> {
        self.stop.send_replace(true);
        let task = self.task.take().ok_or(Error::Closed)?;
        task.await.map_err(|_| Error::Closed)?
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.stop.send_replace(true);
    }
}

pub(crate) async fn connect(
    client: &Client,
    application: &api::ChannelId,
) -> Result<Session, Error> {
    let mut url = client.base.clone();
    let scheme = match url.scheme() {
        "https" => "wss",
        "http" => "ws",
        _ => return Err(Error::Invalid("invalid event transport")),
    };
    url.set_scheme(scheme)
        .map_err(|()| Error::Invalid("invalid event transport"))?;
    url.set_path("/ari/events");
    url.query_pairs_mut()
        .append_pair("app", application.as_str());
    let mut request = url.as_str().into_client_request()?;
    request
        .headers_mut()
        .insert("Authorization", client.authorization.clone());
    request.headers_mut().insert(
        "Sec-WebSocket-Protocol",
        reqwest::header::HeaderValue::from_static("ari"),
    );
    let config = WebSocketConfig::default()
        .max_message_size(Some(65536))
        .max_frame_size(Some(65536))
        .write_buffer_size(0)
        .max_write_buffer_size(131072);
    let (socket, response) = tokio::time::timeout(
        Duration::from_secs(5),
        tokio_tungstenite::connect_async_with_config(request, Some(config), true),
    )
    .await
    .map_err(|_| Error::Timeout)??;
    if response
        .headers()
        .get("Sec-WebSocket-Protocol")
        .is_some_and(|value| value != "ari")
    {
        return Err(Error::Invalid("invalid event subprotocol"));
    }
    let (events, received) = broadcast::channel(64);
    let (stop, stopped) = watch::channel(false);
    let task = tokio::spawn(run(socket, events, stopped));
    Ok(Session {
        events: received,
        stop,
        task: Some(task),
    })
}

async fn run(
    mut socket: Socket,
    events: broadcast::Sender<api::Event>,
    mut stop: watch::Receiver<bool>,
) -> Result<(), Error> {
    let period = Duration::from_secs(20);
    let mut heartbeat = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut last_received = tokio::time::Instant::now();
    let result = async {
        loop {
            if *stop.borrow() { break; }
            tokio::select! {
                changed = stop.changed() => if changed.is_err() || *stop.borrow() { break; },
                message = socket.next() => {
                    let message = message.ok_or(Error::Closed)??;
                    last_received = tokio::time::Instant::now();
                    match message {
                        Message::Text(text) => {
                            let event = serde_json::from_str(&text)?;
                            events.send(event).map_err(|_| Error::Closed)?;
                        }
                        Message::Ping(bytes) => send(&mut socket, Message::Pong(bytes)).await?,
                        Message::Pong(_) => {},
                        Message::Close(_) => return Err(Error::Closed),
                        Message::Binary(_) | Message::Frame(_) => return Err(Error::Invalid("expected a JSON event")),
                    }
                }
                _instant = heartbeat.tick() => {
                    if last_received.elapsed() >= Duration::from_secs(60) { return Err(Error::Timeout); }
                    send(&mut socket, Message::Ping(Vec::new().into())).await?;
                }
            }
        }
        Ok(())
    }.await;
    let close = send(&mut socket, Message::Close(None)).await;
    result.and(close)
}

async fn send(socket: &mut Socket, message: Message) -> Result<(), Error> {
    tokio::time::timeout(Duration::from_secs(2), socket.send(message))
        .await
        .map_err(|_| Error::Timeout)??;
    Ok(())
}
