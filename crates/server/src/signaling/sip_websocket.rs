use crate::{
    http::{self, HttpScheme},
    ControlError,
};
use axum::{
    extract::{
        ws::{rejection::WebSocketUpgradeRejection, WebSocket, WebSocketUpgrade},
        ConnectInfo,
    },
    http::{header, HeaderMap},
    response::Response,
    routing::get,
    Router,
};
use rusterisk_sip::MAX_MESSAGE;
use std::{net::SocketAddr, time::Duration};
use tokio::sync::{mpsc, oneshot};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Scheme {
    Ws,
    Wss,
}

pub(super) struct UpgradeRequest {
    pub scheme: Scheme,
    pub peer: SocketAddr,
    pub origin: Option<String>,
    pub reply: oneshot::Sender<Result<oneshot::Sender<WebSocket>, ControlError>>,
}

#[derive(Clone)]
pub struct SipWebSocketPort {
    requests: mpsc::Sender<UpgradeRequest>,
}

impl SipWebSocketPort {
    pub(super) fn new(requests: mpsc::Sender<UpgradeRequest>) -> Self {
        Self { requests }
    }

    // Signaling closes the upgrade receiver at the end of cleanup.
    #[cfg(test)]
    pub(super) async fn closed(&self) {
        self.requests.closed().await
    }

    async fn reserve(
        &self,
        scheme: Scheme,
        peer: SocketAddr,
        origin: Option<String>,
    ) -> Result<oneshot::Sender<WebSocket>, ControlError> {
        let (reply, result) = oneshot::channel();
        self.requests
            .try_send(UpgradeRequest {
                scheme,
                peer,
                origin,
                reply,
            })
            .map_err(|_| ControlError::Capacity)?;
        tokio::time::timeout(Duration::from_millis(200), result)
            .await
            .map_err(|_| ControlError::Capacity)?
            .map_err(|_| ControlError::Capacity)?
    }

    fn origin(headers: &HeaderMap) -> Result<Option<String>, ControlError> {
        let mut origins = headers.get_all(header::ORIGIN).iter();
        if let Some(origin) = origins.next() {
            if origins.next().is_some() {
                return Err(ControlError::Forbidden);
            }
            let origin = origin.to_str().map_err(|_| ControlError::Forbidden)?;
            if origin.len() > 256 {
                return Err(ControlError::Forbidden);
            }
            return Ok(Some(origin.to_owned()));
        }
        Ok(None)
    }
}

pub(crate) fn router(application: Router, port: SipWebSocketPort, scheme: HttpScheme) -> Router {
    application.route(
        "/ws",
        get(
            move |ConnectInfo(peer): ConnectInfo<SocketAddr>,
                  headers: HeaderMap,
                  upgrade: Result<WebSocketUpgrade, WebSocketUpgradeRejection>| {
                upgrade_socket(port.clone(), scheme, peer, headers, upgrade)
            },
        ),
    )
}

async fn upgrade_socket(
    port: SipWebSocketPort,
    scheme: HttpScheme,
    peer: SocketAddr,
    headers: HeaderMap,
    upgrade: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Response {
    let result = async {
        let origin = SipWebSocketPort::origin(&headers)?;
        let mut sip = false;
        for header in headers.get_all(header::SEC_WEBSOCKET_PROTOCOL) {
            let text = header
                .to_str()
                .map_err(|_| ControlError::Invalid("Invalid WebSocket subprotocol"))?;
            sip |= text.split(',').any(|token| token.trim() == "sip");
        }
        if !sip {
            return Err(ControlError::Invalid(
                "The sip WebSocket subprotocol is required",
            ));
        }
        let upgrade = upgrade.map_err(|_| ControlError::Invalid("WebSocket upgrade required"))?;
        let scheme = match scheme {
            HttpScheme::Http => Scheme::Ws,
            HttpScheme::Https => Scheme::Wss,
        };
        let reserved = port.reserve(scheme, peer, origin).await?;
        Ok(upgrade
            .protocols(["sip"])
            .max_frame_size(MAX_MESSAGE)
            .max_message_size(MAX_MESSAGE)
            .write_buffer_size(0)
            .max_write_buffer_size(MAX_MESSAGE * 2)
            .on_upgrade(move |socket| async move {
                if reserved.send(socket).is_err() {
                    tracing::debug!("Expired signaling upgrade closed");
                }
            }))
    }
    .await;
    match result {
        Ok(response) => response,
        Err(error) => http::error(error),
    }
}
