use axum::{http::StatusCode, response::IntoResponse};
use rusterisk_client::{api, Client, Error};
use rusterisk_server::test_time::Wait;
use std::future::IntoFuture;

#[derive(Clone, Copy)]
struct ReloadReply {
    status: StatusCode,
    message: Option<&'static str>,
}

struct RequestSnapshot {
    method: axum::http::Method,
    path: String,
    query: Option<String>,
    authorized: bool,
    empty_body: bool,
}

struct ReloadExchange {
    result: Result<(), Error>,
    request: Option<RequestSnapshot>,
}

async fn exchange(reply: ReloadReply) -> Result<ReloadExchange, Error> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let client = Client::new(&format!("http://{address}"), "app", "password")?;
    let (sender, mut observed) = tokio::sync::mpsc::channel(1);
    let app = axum::Router::new().fallback(axum::routing::any(
        move |request: axum::extract::Request| {
            let sender = sender.clone();
            async move {
                let (parts, body) = request.into_parts();
                let snapshot = RequestSnapshot {
                    method: parts.method,
                    path: parts.uri.path().to_owned(),
                    query: parts.uri.query().map(str::to_owned),
                    authorized: parts
                        .headers
                        .get(axum::http::header::AUTHORIZATION)
                        .is_some_and(|value| value == "Basic YXBwOnBhc3N3b3Jk"),
                    empty_body: axum::body::to_bytes(body, 0).await.is_ok(),
                };
                sender
                    .try_send(snapshot)
                    .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
                let response = match reply.message {
                    Some(message) => (
                        reply.status,
                        axum::Json(api::ApiError {
                            message: message.to_owned(),
                        }),
                    )
                        .into_response(),
                    None => reply.status.into_response(),
                };
                Ok::<_, StatusCode>(response)
            }
        },
    ));
    let server = tokio::spawn(axum::serve(listener, app).into_future());
    let result = match Wait::Ready.bounded(client.reload_configuration()).await {
        Ok(result) => result,
        Err(_) => Err(Error::Timeout),
    };
    server.abort();
    match server.await {
        Err(error) if error.is_cancelled() => {}
        Ok(Ok(())) => {}
        cleanup @ (Ok(Err(_)) | Err(_)) => {
            let primary = match &result {
                Ok(()) => "reload completed".to_owned(),
                Err(error) => error.to_string(),
            };
            return Err(Error::Cleanup(format!(
                "reload: {primary}; fixture cleanup: {cleanup:?}"
            )));
        }
    }
    Ok(ReloadExchange {
        result,
        request: observed.try_recv().ok(),
    })
}

#[tokio::test]
async fn reload_uses_authenticated_management_metadata_with_an_empty_request() -> Result<(), Error>
{
    let exchange = exchange(ReloadReply {
        status: StatusCode::NO_CONTENT,
        message: None,
    })
    .await?;
    exchange.result?;
    let request = exchange
        .request
        .ok_or(Error::Invalid("reload fixture received no request"))?;
    let metadata = api::ManagementOperation::ReloadConfiguration.metadata();
    assert_eq!(request.method.as_str(), metadata.method.upper());
    assert_eq!(request.path, metadata.path);
    assert!(request.query.is_none());
    assert!(request.authorized);
    assert!(request.empty_body);
    Ok(())
}

#[tokio::test]
async fn accepted_reload_does_not_report_applied_configuration() -> Result<(), Error> {
    let exchange = exchange(ReloadReply {
        status: StatusCode::ACCEPTED,
        message: None,
    })
    .await?;
    assert!(matches!(
        exchange.result,
        Err(Error::Api { status: 202, .. })
    ));
    Ok(())
}

#[tokio::test]
async fn reload_preserves_remote_status_and_api_error_message() -> Result<(), Error> {
    for (status, expected) in [
        (
            StatusCode::BAD_REQUEST,
            "pjsip.conf:9: missing endpoint auth",
        ),
        (StatusCode::UNAUTHORIZED, "Authentication required"),
        (StatusCode::FORBIDDEN, "Read-only user"),
        (StatusCode::CONFLICT, "Configuration reload is busy"),
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "Reload source is unavailable",
        ),
    ] {
        let exchange = exchange(ReloadReply {
            status,
            message: Some(expected),
        })
        .await?;
        assert!(
            matches!(exchange.result, Err(Error::Api { status: actual, message }) if actual == status.as_u16() && message.message == expected),
            "reload status {status}"
        );
    }
    Ok(())
}
