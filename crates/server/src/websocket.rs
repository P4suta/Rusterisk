use crate::{http, ControlError, Controller, Subscription};
use axum::extract::ws::{
    rejection::WebSocketUpgradeRejection, Message, WebSocket, WebSocketUpgrade,
};
use axum::extract::State;
use axum::http::{HeaderMap, Uri};
use axum::response::Response;
use std::time::{Duration, Instant};

pub(crate) async fn upgrade(
    State(controller): State<Controller>,
    uri: Uri,
    headers: HeaderMap,
    upgrade: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Response {
    let result = async {
        let parameters = http::parameters(&uri)?;
        let credentials = http::credentials(&headers, &parameters)?;
        let origin = headers
            .get("Origin")
            .map(|value| value.to_str().map(String::from))
            .transpose()
            .map_err(|_| ControlError::Forbidden)?;
        controller
            .authorize(credentials.clone(), origin.clone(), true)
            .await?;
        let upgrade = upgrade.map_err(|_| ControlError::Invalid("WebSocket upgrade required"))?;
        let application = parameters
            .get("app")
            .ok_or(ControlError::Invalid("Missing required parameter"))?
            .clone();
        if parameters
            .get("subscribeAll")
            .is_some_and(|value| value != "false")
        {
            return Err(ControlError::Unsupported);
        }
        let subscription = controller
            .subscribe(credentials, origin, application)
            .await?;
        Ok(upgrade
            .protocols(["ari"])
            .max_frame_size(16384)
            .max_message_size(16384)
            .write_buffer_size(0)
            .max_write_buffer_size(32768)
            .on_upgrade(|socket| events(socket, subscription)))
    }
    .await;
    match result {
        Ok(response) => response,
        Err(error) => http::error(error),
    }
}

async fn send(socket: &mut WebSocket, message: Message) -> bool {
    match tokio::time::timeout(Duration::from_secs(2), socket.send(message)).await {
        Ok(Ok(())) => true,
        Ok(Err(_)) | Err(_) => false,
    }
}

async fn events(mut socket: WebSocket, mut subscription: Subscription) {
    let mut last_received = Instant::now();
    let period = Duration::from_secs(20);
    let mut heartbeat = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            event = subscription.events.recv() => {
                let Some(event) = event else { break; };
                let text = match serde_json::to_string(&event) {
                    Ok(text) => text,
                    Err(error) => {
                        tracing::error!(application = %subscription.application, %error, "event serialization failed");
                        break;
                    }
                };
                if !send(&mut socket, Message::Text(text.into())).await { break; }
            }
            message = socket.recv() => {
                last_received = Instant::now();
                match message {
                    Some(Ok(Message::Ping(bytes))) => {
                        if !send(&mut socket, Message::Pong(bytes)).await { break; }
                    }
                    Some(Ok(Message::Pong(_))) => {}
                    Some(Ok(Message::Text(_) | Message::Binary(_) | Message::Close(_)))
                    | Some(Err(_)) | None => break,
                }
            }
            _instant = heartbeat.tick() => {
                if last_received.elapsed() > Duration::from_secs(60)
                    || !send(&mut socket, Message::Ping(axum::body::Bytes::new())).await {
                    break;
                }
            }
        }
    }
    drop(subscription);
    if !send(&mut socket, Message::Close(None)).await {
        tracing::debug!("event connection closed before the close frame was sent");
    }
}
