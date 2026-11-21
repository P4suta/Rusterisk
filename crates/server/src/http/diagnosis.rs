use super::{credentials, error, id};
use crate::{runner::DiagnosisTarget, ControlError, Controller};
use axum::{
    extract::{Path, State},
    http::{header, HeaderMap},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use rusterisk_api::ManagementOperation;
use std::collections::BTreeMap;

pub(super) fn router(controller: Controller) -> Router {
    Router::new()
        .route(
            ManagementOperation::DiagnoseExtension.metadata().path,
            get(extension),
        )
        .route(
            ManagementOperation::DiagnoseChannel.metadata().path,
            get(channel),
        )
        .with_state(controller)
}

async fn extension(
    State(controller): State<Controller>,
    Path((context, extension)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let target = if [&context, &extension].iter().any(|s| {
        s.is_empty() || s.len() > 256 || s.contains('/') || s.chars().any(char::is_control)
    }) {
        Err(ControlError::Invalid("Invalid dialplan location"))
    } else {
        Ok(DiagnosisTarget::Extension { context, extension })
    };
    read(controller, &headers, target).await
}

async fn channel(
    State(controller): State<Controller>,
    Path(channel): Path<String>,
    headers: HeaderMap,
) -> Response {
    read(
        controller,
        &headers,
        id(&channel).map(DiagnosisTarget::Channel),
    )
    .await
}

async fn read(
    controller: Controller,
    headers: &HeaderMap,
    target: Result<DiagnosisTarget, ControlError>,
) -> Response {
    let result = async {
        let auth = credentials(headers, &BTreeMap::new())?;
        let origin = headers
            .get(header::ORIGIN)
            .map(|value| value.to_str().map(str::to_owned))
            .transpose()
            .map_err(|_| ControlError::Forbidden)?;
        controller.diagnose(auth, origin, target?).await
    }
    .await;
    match result {
        Ok(value) => Json(value).into_response(),
        Err(problem) => error(problem),
    }
}
