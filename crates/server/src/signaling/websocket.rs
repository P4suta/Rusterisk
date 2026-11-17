use super::{failure, Signaling};
use crate::{
    encryption::TlsClientContext, media_websocket::MediaLinkOptions, CallKey, ControlError,
    Milestone, Signal,
};
use base64::Engine;
use rusterisk_api::{ChannelId, WebSocketMediaOptions};
use rusterisk_config::WebSocketClient;
use std::{collections::BTreeMap, time::Duration};
use tokio::sync::watch;
use tokio_tungstenite::{
    tungstenite::{client::IntoClientRequest, http::HeaderValue, protocol::WebSocketConfig},
    MaybeTlsStream, WebSocketStream,
};
#[cfg(test)]
mod tests;

type ClientSocket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

pub(super) struct PendingWebSocket {
    pub(super) cancel: watch::Sender<bool>,
    pub(super) received: bool,
    pub(super) answered: bool,
}

pub(super) struct Completion {
    key: CallKey,
    result: Result<(), ControlError>,
}

#[derive(Clone)]
pub(crate) struct PreparedClients(Vec<(WebSocketClient, Option<TlsClientContext>)>);

pub(crate) struct PreparedWebSocketClient {
    client: WebSocketClient,
    context: Option<TlsClientContext>,
}

impl PreparedClients {
    pub(crate) fn new(clients: &[WebSocketClient]) -> Result<Self, ControlError> {
        let mut prepared = Vec::with_capacity(clients.len());
        for client in clients {
            let tls = if client.requires_tls() {
                Some(
                    TlsClientContext::load(client.ca_list_file.as_deref()).map_err(|error| {
                        ControlError::Adapter(format!(
                            "WebSocket TLS configuration failed: {error}"
                        ))
                    })?,
                )
            } else {
                None
            };
            prepared.push((client.clone(), tls));
        }
        Ok(Self(prepared))
    }

    pub(crate) fn capture(
        &self,
        client: &WebSocketClient,
    ) -> Result<PreparedWebSocketClient, ControlError> {
        self.0
            .iter()
            .find(|(settings, _tls)| settings == client)
            .map(|(settings, tls)| PreparedWebSocketClient {
                client: settings.clone(),
                context: tls.clone(),
            })
            .ok_or(ControlError::Invalid("WebSocket client was not prepared"))
    }

    pub(crate) fn same(&self, other: &Self) -> bool {
        self.0.len() == other.0.len()
            && self
                .0
                .iter()
                .zip(&other.0)
                .all(|((first, first_tls), (second, second_tls))| {
                    first == second
                        && match (first_tls, second_tls) {
                            (Some(first), Some(second)) => first.same_profile(second),
                            (None, None) => true,
                            (Some(_), None) | (None, Some(_)) => false,
                        }
                })
    }
}

async fn connect(
    client: &WebSocketClient,
    tls: Option<TlsClientContext>,
) -> Result<ClientSocket, ControlError> {
    let mut remaining = client.reconnect_attempts;
    loop {
        let mut request = client
            .uri
            .expose()
            .into_client_request()
            .map_err(|_| ControlError::Invalid("Invalid WebSocket client URI"))?;
        request
            .headers_mut()
            .insert("Sec-WebSocket-Protocol", HeaderValue::from_static("media"));
        if let (Some(username), Some(password)) = (&client.username, &client.password) {
            let encoded = base64::engine::general_purpose::STANDARD.encode(format!(
                "{}:{}",
                username.expose(),
                password.expose()
            ));
            request.headers_mut().insert(
                "Authorization",
                HeaderValue::from_str(&format!("Basic {encoded}"))
                    .map_err(|_| ControlError::Invalid("Invalid WebSocket client credentials"))?,
            );
        }
        let limits = WebSocketConfig::default()
            .read_buffer_size(65536)
            .write_buffer_size(0)
            .max_write_buffer_size(65536)
            .max_message_size(Some(65500))
            .max_frame_size(Some(65500));
        let attempt = tokio::time::timeout(
            Duration::from_millis(u64::from(client.connection_timeout_ms)),
            tokio_tungstenite::connect_async_tls_with_config(
                request,
                Some(limits),
                true,
                tls.as_ref().map(TlsClientContext::websocket_connector),
            ),
        )
        .await;
        if let Ok(Ok((socket, response))) = attempt {
            if response.headers().get("Sec-WebSocket-Protocol")
                == Some(&HeaderValue::from_static("media"))
            {
                return Ok(socket);
            }
        }
        remaining = remaining.saturating_sub(1);
        if remaining == 0 {
            return Err(ControlError::Adapter("WebSocket connection failed".into()));
        }
        tokio::time::sleep(Duration::from_millis(u64::from(
            client.reconnect_interval_ms,
        )))
        .await;
    }
}

impl Signaling {
    pub(super) fn start_websocket(
        &mut self,
        key: CallKey,
        prepared: PreparedWebSocketClient,
        options: WebSocketMediaOptions,
        channel_variables: BTreeMap<String, String>,
    ) -> Result<(), ControlError> {
        if self.calls.len()
            + self.external_media.len()
            + self.stream_admissions.len()
            + self.websocket_admissions.len()
            >= 64
            || self.websocket_jobs.len() >= 64
        {
            return Err(ControlError::Capacity);
        }
        let PreparedWebSocketClient {
            client,
            context: tls,
        } = prepared;
        let options = MediaLinkOptions {
            connection_id: ChannelId::try_from(client.name.clone())?,
            channel_id: key.id.clone(),
            channel: format!("WebSocket/{}/{:08x}", client.name, key.generation),
            format: options.format,
            control_format: options.control_format,
            direction: options.direction,
            auto_answer: options.auto_answer,
            channel_variables,
        };
        let (cancel, mut stopped) = watch::channel(false);
        self.websocket_admissions.insert(
            key.clone(),
            PendingWebSocket {
                cancel,
                received: false,
                answered: false,
            },
        );
        let media = self.media.clone();
        self.websocket_jobs.spawn(async move {
            let connection = async {
                let socket = connect(&client, tls).await?;
                media.attach_client_websocket(key.clone(), socket, options).await.map_err(failure)
            };
            let mut result = tokio::select! {
                result = tokio::time::timeout(Duration::from_secs(4), connection) => result.map_err(|_| ControlError::Adapter("WebSocket admission timed out".into())).and_then(|result| result),
                _changed = stopped.changed() => Err(ControlError::Adapter("WebSocket admission cancelled".into())),
            };
            if let Err(primary) = &result {
                if let Err(cleanup) = media.release(key.clone()) { result = Err(failure(format!("{primary}; WebSocket admission cleanup failed: {cleanup}"))); }
            }
            Completion { key, result }
        });
        Ok(())
    }

    pub(super) async fn websocket_completed(
        &mut self,
        completed: Completion,
    ) -> Result<(), ControlError> {
        let Some(pending) = self.websocket_admissions.remove(&completed.key) else {
            self.media.release(completed.key).map_err(failure)?;
            return Ok(());
        };
        let admitted = completed.result.is_ok();
        if admitted {
            self.external_media.insert(completed.key.clone());
        }
        self.notify(Signal::WebSocketAdmitted {
            key: completed.key.clone(),
            result: completed.result,
        })
        .await?;
        if admitted {
            self.notify(Signal::DtmfReady {
                key: completed.key.clone(),
                events: u16::MAX,
            })
            .await?;
            if pending.answered {
                self.notify(Signal::Milestone {
                    key: completed.key.clone(),
                    milestone: Milestone::Answered,
                })
                .await?;
            }
            if pending.received {
                self.notify(Signal::Milestone {
                    key: completed.key,
                    milestone: Milestone::MediaConnected,
                })
                .await?;
            }
        }
        Ok(())
    }

    pub(super) async fn finish_websockets(&mut self) -> Result<(), ControlError> {
        for (_, pending) in std::mem::take(&mut self.websocket_admissions) {
            pending.cancel.send_replace(true);
        }
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while !self.websocket_jobs.is_empty() {
            match tokio::time::timeout_at(deadline, self.websocket_jobs.join_next()).await {
                Ok(Some(Ok(completed))) => {
                    if let Err(error) = completed.result {
                        tracing::debug!(channel = %completed.key.id, generation = completed.key.generation, %error, "WebSocket admission stopped during shutdown");
                    }
                }
                Ok(Some(Err(error))) => return Err(failure(error)),
                Ok(None) => break,
                Err(error) => {
                    self.websocket_jobs.abort_all();
                    while let Some(result) = self.websocket_jobs.join_next().await {
                        match result {
                            Ok(_) => {}
                            Err(error) if error.is_cancelled() => {}
                            Err(error) => return Err(failure(error)),
                        }
                    }
                    return Err(failure(error));
                }
            }
        }
        Ok(())
    }
}
