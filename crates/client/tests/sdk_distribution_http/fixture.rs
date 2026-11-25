use axum::{
    body::Body,
    extract::{Query, State},
    http::{HeaderMap, Method, StatusCode, Uri},
    response::{IntoResponse, Response},
    Json, Router,
};
use rusterisk_client::{api, Client};
use std::{collections::BTreeMap, future::IntoFuture, time::Duration};
use tokio::sync::{mpsc, oneshot};

const ID: &str = "sdk-distribution-run";

#[derive(Clone, Copy, Eq, PartialEq)]
pub enum Mode {
    Successful,
    Preexisting,
    LostCreationResponse,
    ListAndCleanupFailure,
    WrongVersion,
    RejectedCreation,
    CreationConflict,
    WrongCreatedResponse,
    DeleteIgnored,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Step {
    Ping,
    Info,
    Inspect,
    Create,
    List,
    Delete,
}

pub struct Report {
    pub steps: Vec<Step>,
    pub exists: bool,
}

struct Request {
    step: Step,
    response: oneshot::Sender<Response>,
}

fn error(status: StatusCode) -> Response {
    (
        status,
        Json(api::ApiError {
            message: "fixture-password must remain absent from diagnostics".to_owned(),
        }),
    )
        .into_response()
}

async fn request(
    State(sender): State<mpsc::Sender<Request>>,
    Query(query): Query<BTreeMap<String, String>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    if headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        != Some("Basic YXBpOmZpeHR1cmUtcGFzc3dvcmQ=")
    {
        return error(StatusCode::UNAUTHORIZED);
    }
    let step = match (method, uri.path()) {
        (Method::GET, "/ari/asterisk/ping") if query.is_empty() => Step::Ping,
        (Method::GET, "/ari/asterisk/info")
            if query.len() == 1
                && query.get("only").map(String::as_str) == Some("system,status") =>
        {
            Step::Info
        }
        (Method::GET, "/ari/bridges/sdk-distribution-run") if query.is_empty() => Step::Inspect,
        (Method::POST, "/ari/bridges/sdk-distribution-run")
            if query.len() == 1
                && query.get("name").map(String::as_str) == Some("SDK distribution consumer") =>
        {
            Step::Create
        }
        (Method::GET, "/ari/bridges") if query.is_empty() => Step::List,
        (Method::DELETE, "/ari/bridges/sdk-distribution-run") if query.is_empty() => Step::Delete,
        _ => return error(StatusCode::BAD_REQUEST),
    };
    let (response, receiver) = oneshot::channel();
    if sender.send(Request { step, response }).await.is_err() {
        return error(StatusCode::SERVICE_UNAVAILABLE);
    }
    receiver
        .await
        .unwrap_or_else(|_| error(StatusCode::SERVICE_UNAVAILABLE))
}

fn bridge() -> Result<api::Bridge, std::io::Error> {
    Ok(api::Bridge {
        id: api::ChannelId::try_from(ID.to_owned()).map_err(std::io::Error::other)?,
        technology: "simple_bridge".to_owned(),
        bridge_type: "mixing".to_owned(),
        bridge_class: "stasis".to_owned(),
        creator: "Stasis".to_owned(),
        name: "SDK distribution consumer".to_owned(),
        channels: Vec::new(),
        creationtime: "2026-11-16T00:00:00Z".to_owned(),
        video_mode: None,
        video_source_id: None,
    })
}

async fn respond(
    mut requests: mpsc::Receiver<Request>,
    mode: Mode,
) -> Result<Report, std::io::Error> {
    let bridge = bridge()?;
    let mut report = Report {
        steps: Vec::new(),
        exists: mode == Mode::Preexisting,
    };
    while let Some(request) = requests.recv().await {
        if report.steps.len() == 16 {
            return Err(std::io::Error::other("fixture request bound exceeded"));
        }
        report.steps.push(request.step);
        let response = match request.step {
            Step::Ping => Json(api::AsteriskPing {
                ping: "pong".to_owned(),
                timestamp: "2026-11-16T00:00:00Z".to_owned(),
                asterisk_id: "fixture".to_owned(),
            })
            .into_response(),
            Step::Info => Json(api::AsteriskInfo {
                system: Some(api::SystemInfo {
                    version: if mode == Mode::WrongVersion {
                        "0.2.0"
                    } else {
                        "0.1.0"
                    }
                    .to_owned(),
                    entity_id: "fixture".to_owned(),
                }),
                status: Some(api::StatusInfo {
                    startup_time: "2026-11-16T00:00:00Z".to_owned(),
                    last_reload_time: "2026-11-16T00:00:00Z".to_owned(),
                }),
                ..api::AsteriskInfo::default()
            })
            .into_response(),
            Step::Inspect if report.exists => Json(bridge.clone()).into_response(),
            Step::Inspect => error(StatusCode::NOT_FOUND),
            Step::Create => {
                report.exists = mode != Mode::RejectedCreation;
                if mode == Mode::RejectedCreation {
                    error(StatusCode::INTERNAL_SERVER_ERROR)
                } else if mode == Mode::CreationConflict {
                    error(StatusCode::CONFLICT)
                } else if mode == Mode::WrongCreatedResponse {
                    let mut wrong = bridge.clone();
                    wrong.id = api::ChannelId::try_from("another-bridge".to_owned())
                        .map_err(std::io::Error::other)?;
                    Json(wrong).into_response()
                } else if mode == Mode::LostCreationResponse {
                    let stream =
                        futures_util::stream::once(std::future::ready(Err::<&'static str, _>(
                            std::io::Error::other("injected response loss"),
                        )));
                    Response::new(Body::from_stream(stream))
                } else {
                    Json(bridge.clone()).into_response()
                }
            }
            Step::List if mode == Mode::ListAndCleanupFailure => {
                error(StatusCode::INTERNAL_SERVER_ERROR)
            }
            Step::List => Json(if report.exists {
                std::iter::once(bridge.clone()).collect::<Vec<_>>()
            } else {
                Vec::new()
            })
            .into_response(),
            Step::Delete if mode == Mode::ListAndCleanupFailure => {
                error(StatusCode::INTERNAL_SERVER_ERROR)
            }
            Step::Delete if mode == Mode::DeleteIgnored => StatusCode::NO_CONTENT.into_response(),
            Step::Delete if !report.exists => error(StatusCode::NOT_FOUND),
            Step::Delete => {
                report.exists = false;
                StatusCode::NO_CONTENT.into_response()
            }
        };
        request
            .response
            .send(response)
            .map_err(|_| std::io::Error::other("fixture response receiver closed"))?;
    }
    Ok(report)
}

pub async fn run(
    mode: Mode,
) -> Result<(Result<(), crate::http_contract::HttpContractError>, Report), std::io::Error> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let (sender, receiver) = mpsc::channel(8);
    let mut worker = tokio::spawn(respond(receiver, mode));
    let router = Router::new().fallback(request).with_state(sender.clone());
    let (shutdown, stop) = oneshot::channel();
    let mut server = tokio::spawn(
        axum::serve(listener, router)
            .with_graceful_shutdown(async move {
                if let Err(error) = stop.await {
                    eprintln!("fixture shutdown channel: {error}");
                }
            })
            .into_future(),
    );
    let operation = async {
        let client = Client::new(&format!("http://{address}"), "api", "fixture-password")
            .map_err(std::io::Error::other)?;
        let identifier =
            rusterisk_client::BridgeId::try_from(ID.to_owned()).map_err(std::io::Error::other)?;
        Ok::<_, std::io::Error>(crate::http_contract::verify(&client, "0.1.0", &identifier).await)
    }
    .await;
    let signal = shutdown
        .send(())
        .map_err(|_| std::io::Error::other("fixture server ended before shutdown"));
    let serving = match tokio::time::timeout(Duration::from_secs(5), &mut server).await {
        Ok(result) => result
            .map_err(std::io::Error::other)
            .and_then(|result| result),
        Err(_) => {
            server.abort();
            let stopped = server.await;
            if stopped.is_err_and(|error| error.is_cancelled()) {
                Err(std::io::Error::other("fixture server shutdown expired"))
            } else {
                Err(std::io::Error::other("fixture server cleanup failed"))
            }
        }
    };
    drop(sender);
    let report = match tokio::time::timeout(Duration::from_secs(5), &mut worker).await {
        Ok(result) => result
            .map_err(std::io::Error::other)
            .and_then(|result| result),
        Err(_) => {
            worker.abort();
            if worker.await.is_err_and(|error| error.is_cancelled()) {
                Err(std::io::Error::other("fixture worker shutdown expired"))
            } else {
                Err(std::io::Error::other("fixture worker cleanup failed"))
            }
        }
    };
    let failures: Vec<_> = [
        signal.as_ref().err(),
        serving.as_ref().err(),
        report.as_ref().err(),
    ]
    .into_iter()
    .flatten()
    .map(ToString::to_string)
    .collect();
    if !failures.is_empty() {
        let primary = match &operation {
            Ok(Ok(())) => "successful HTTP operation".to_owned(),
            Ok(Err(error)) => error.to_string(),
            Err(error) => error.to_string(),
        };
        return Err(std::io::Error::other(format!(
            "{primary}; fixture cleanup: {}",
            failures.join("; ")
        )));
    }
    Ok((operation?, report?))
}
