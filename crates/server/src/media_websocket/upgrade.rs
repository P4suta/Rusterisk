use crate::{http, media::MediaPort, ControlError, Controller};
use axum::{
    extract::ws::{rejection::WebSocketUpgradeRejection, WebSocketUpgrade},
    http::{HeaderMap, Uri},
    response::Response,
};
use rusterisk_api::ChannelId;

pub(crate) async fn upgrade(
    controller: Controller,
    media: MediaPort,
    uri: Uri,
    headers: HeaderMap,
    upgrade: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Response {
    let result = async {
        let _parameters = http::parameters(&uri)?;
        let origin = headers
            .get("Origin")
            .map(|value| value.to_str().map(str::to_owned))
            .transpose()
            .map_err(|_| ControlError::Forbidden)?;
        let protocol = headers
            .get("Sec-WebSocket-Protocol")
            .and_then(|value| value.to_str().ok())
            .filter(|value| value.len() <= 256)
            .is_some_and(|value| value.split(',').any(|protocol| protocol.trim() == "media"));
        if !protocol {
            return Err(ControlError::Invalid("Media WebSocket protocol required"));
        }
        let upgrade = upgrade.map_err(|_| ControlError::Invalid("WebSocket upgrade required"))?;
        let connection_id = ChannelId::try_from(
            uri.path()
                .strip_prefix("/media/")
                .ok_or(ControlError::NotFound("WebSocket instance not found"))?
                .to_owned(),
        )?;
        let claimed = controller
            .claim_media_connection(connection_id, origin)
            .await?;
        Ok(upgrade
            .protocols(["media"])
            .max_frame_size(65500)
            .max_message_size(65500)
            .write_buffer_size(0)
            .max_write_buffer_size(65536)
            .on_upgrade(async move |socket| {
                if let Err(error) = media.attach_claimed_websocket(claimed, socket).await {
                    tracing::debug!(%error, "media WebSocket attachment failed");
                }
            }))
    }
    .await;
    match result {
        Ok(response) => response,
        Err(error) => http::error(error),
    }
}
