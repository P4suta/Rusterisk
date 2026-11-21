use super::{credentials, error, id};
use crate::{media::MediaPort, ControlError, Controller};
use axum::{
    extract::{Path, State},
    http::{HeaderMap, Uri},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use rusterisk_api::{ManagementOperation, RtpObservation};
use std::collections::BTreeMap;

pub(super) fn router(controller: Controller, media: Option<MediaPort>) -> Router {
    Router::new()
        .route(
            ManagementOperation::ChannelRtpObservation.metadata().path,
            get(read),
        )
        .with_state((controller, media))
}

async fn read(
    State((controller, media)): State<(Controller, Option<MediaPort>)>,
    Path(channel): Path<String>,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    let result = async {
        let auth = credentials(&headers, &BTreeMap::new())?;
        let origin = headers
            .get("Origin")
            .map(|value| value.to_str().map(str::to_owned))
            .transpose()
            .map_err(|_| ControlError::Forbidden)?;
        let id = id(&channel)?;
        let (key, creationtime) = controller
            .observed_channel(auth.clone(), origin.clone(), id.clone())
            .await?;
        if uri.query().is_some() {
            return Err(ControlError::Invalid(
                "The RTP observation takes no query parameters",
            ));
        }
        let measurements = match media {
            Some(port) => port.measurements(key.clone()).await?,
            None => None,
        };
        let (observed_at, local, peer) = match measurements {
            Some((captured, sample)) => (captured, Some(sample.local), sample.peer),
            None => (crate::utc_timestamps::now_millis(), None, None),
        };
        if controller.observed_channel(auth, origin, id).await?.0 != key {
            return Err(ControlError::Conflict(
                "Channel lifetime changed during observation",
            ));
        }
        Ok(RtpObservation {
            channel_id: key.id,
            generation: key.generation,
            creationtime,
            observed_at,
            local,
            peer,
        })
    }
    .await;
    match result {
        Ok(snapshot) => Json(snapshot).into_response(),
        Err(problem) => error(problem),
    }
}
