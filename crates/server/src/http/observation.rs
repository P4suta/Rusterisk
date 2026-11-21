use super::{credentials, error, id};
use crate::{runner::WatchTarget, ControlError, Controller};
use axum::{
    body::{Body, Bytes},
    extract::{Path, State},
    http::{header, HeaderMap},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use rusterisk_api::observation::{CHANNEL_PATH, CONTENT_TYPE, ENDPOINT_PATH};
use std::collections::BTreeMap;

pub(super) fn router(controller: Controller) -> Router {
    Router::new()
        .route(CHANNEL_PATH, get(channel))
        .route(ENDPOINT_PATH, get(endpoint))
        .with_state(controller)
}

async fn channel(
    State(controller): State<Controller>,
    Path(channel): Path<String>,
    headers: HeaderMap,
) -> Response {
    stream(controller, &headers, id(&channel).map(WatchTarget::Channel)).await
}

async fn endpoint(
    State(controller): State<Controller>,
    Path((technology, resource)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let target = id(&technology)
        .and_then(|_| id(&resource))
        .map(|_| WatchTarget::Endpoint {
            technology,
            resource,
        });
    stream(controller, &headers, target).await
}

async fn stream(
    controller: Controller,
    headers: &HeaderMap,
    target: Result<WatchTarget, ControlError>,
) -> Response {
    let observe = async {
        let auth = credentials(headers, &BTreeMap::new())?;
        let origin = headers
            .get(header::ORIGIN)
            .map(|origin| origin.to_str().map(str::to_owned))
            .transpose()
            .map_err(|_| ControlError::Forbidden)?;
        controller.observe(auth, origin, target?).await
    }
    .await;
    let stream = match observe {
        Ok(stream) => stream,
        Err(failure) => return error(failure),
    };
    let records = futures_util::stream::unfold(stream, |mut stream| async move {
        let record = stream.next().await?;
        let bytes = serde_json::to_vec(&record).map(|mut bytes| {
            bytes.push(b'\n');
            Bytes::from(bytes)
        });
        Some((bytes, stream))
    });
    (
        [
            (header::CONTENT_TYPE, CONTENT_TYPE),
            (header::CACHE_CONTROL, "no-store"),
        ],
        Body::from_stream(records),
    )
        .into_response()
}
