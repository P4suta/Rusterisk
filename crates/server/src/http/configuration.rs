use super::{credentials, error};
use crate::{ControlError, Controller};
use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use rusterisk_api::ManagementOperation;
use serde_json::json;
use std::{collections::BTreeMap, time::Duration};
use tower_http::timeout::TimeoutLayer;

pub(super) fn router(controller: Controller) -> Router {
    let operation = ManagementOperation::ReloadConfiguration.metadata();
    Router::new()
        .route(operation.path, post(reload))
        .layer(DefaultBodyLimit::max(16384))
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            Duration::from_secs(30),
        ))
        .with_state(controller)
}

async fn reload(
    State(controller): State<Controller>,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let result = async {
        let credentials = credentials(&headers, &BTreeMap::new())?;
        let origin = headers
            .get("Origin")
            .map(|value| value.to_str().map(String::from))
            .transpose()
            .map_err(|_| ControlError::Forbidden)?;
        controller
            .authorize(credentials.clone(), origin.clone(), false)
            .await?;
        if uri.query().is_some() || !body.is_empty() {
            return Err(ControlError::Invalid(
                "Configuration reload requires an empty body and query",
            ));
        }
        controller.reload_startup(credentials, origin).await
    }
    .await;
    match result {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(ControlError::Configuration(configuration)) => {
            let message = configuration.to_string();
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({
                    "message": message,
                    "file": configuration.file,
                    "line": configuration.line,
                    "reason": configuration.reason,
                })),
            )
                .into_response()
        }
        Err(failure) => error(failure),
    }
}
