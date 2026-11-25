use axum::{
    extract::{Query, State},
    http::{HeaderMap, Method, StatusCode},
};
use rusterisk_client::{
    api::{ApiError, ChannelId},
    Client, Error,
};
use rusterisk_server::test_time::Wait;
use std::{collections::BTreeMap, future::IntoFuture};
use tokio::sync::{mpsc, oneshot};

struct Request {
    method: Method,
    query: BTreeMap<String, String>,
}

async fn capture(
    State(observed): State<mpsc::Sender<Request>>,
    method: Method,
    headers: HeaderMap,
    Query(query): Query<BTreeMap<String, String>>,
) -> Result<StatusCode, StatusCode> {
    if headers
        .get("Authorization")
        .and_then(|value| value.to_str().ok())
        != Some("Basic YXBwOnBhc3N3b3Jk")
    {
        return Err(StatusCode::UNAUTHORIZED);
    }
    observed
        .try_send(Request { method, query })
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    Ok(StatusCode::NO_CONTENT)
}

fn combine(primary: Result<(), Error>, cleanup: Result<(), Error>) -> Result<(), Error> {
    match (primary, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(primary), Err(cleanup)) => Err(Error::Cleanup(format!(
            "{primary}; music client cleanup failed: {cleanup}"
        ))),
    }
}

#[tokio::test]
async fn music_controls_preserve_optional_class_query_encoding_authentication_and_api_failures(
) -> Result<(), Error> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let (observed, mut requests) = mpsc::channel(3);
    let app = axum::Router::new()
        .route(
            "/ari/channels/phone_123/moh",
            axum::routing::post(capture).delete(capture),
        )
        .route(
            "/ari/channels/missing/moh",
            axum::routing::post(|| async {
                (
                    StatusCode::NOT_FOUND,
                    axum::Json(ApiError {
                        message: "Channel not found".into(),
                    }),
                )
            }),
        )
        .with_state(observed);
    let (stop, stopped) = oneshot::channel();
    let mut serving = tokio::spawn(
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                if stopped.await.is_err() {
                    tracing::debug!("music client test HTTP stop sender disconnected");
                }
            })
            .into_future(),
    );
    let result = Wait::Ready
        .bounded(async {
            let client = Client::new(&format!("http://{address}"), "app", "password")?;
            let channel = ChannelId::try_from("phone_123".to_owned())?;
            client.start_music_on_hold(&channel, None).await?;
            let class = "known /音楽?&";
            client.start_music_on_hold(&channel, Some(class)).await?;
            client.stop_music_on_hold(&channel).await?;
            for (method, class) in [
                (Method::POST, None),
                (Method::POST, Some(class)),
                (Method::DELETE, None),
            ] {
                let request = requests.recv().await.ok_or(Error::Closed)?;
                if request.method != method
                    || request.query.len() != usize::from(class.is_some())
                    || request.query.get("mohClass").map(String::as_str) != class
                {
                    return Err(Error::Invalid(
                        "music client changed the method or optional class query",
                    ));
                }
            }
            let missing = ChannelId::try_from("missing".to_owned())?;
            if !matches!(client.start_music_on_hold(&missing, None).await,
            Err(Error::Api { status: 404, message }) if message.message == "Channel not found")
            {
                return Err(Error::Invalid("music client lost the API failure"));
            }
            Ok(())
        })
        .await
        .map_err(|_| Error::Timeout)
        .and_then(|result| result);
    let result = combine(result, stop.send(()).map_err(|_| Error::Closed));
    let cleanup = match Wait::Ready.bounded(&mut serving).await {
        Ok(result) => match result {
            Ok(result) => result.map_err(Error::Io),
            Err(error) => Err(Error::Cleanup(error.to_string())),
        },
        Err(primary) => {
            serving.abort();
            let cancelled = match serving.await {
                Err(error) if error.is_cancelled() => Ok(()),
                Err(error) => Err(Error::Cleanup(error.to_string())),
                Ok(result) => result.map_err(Error::Io),
            };
            combine(Err(Error::Cleanup(primary.to_string())), cancelled)
        }
    };
    let result = combine(result, cleanup);
    combine(
        result,
        tokio::net::TcpListener::bind(address)
            .await
            .map(drop)
            .map_err(Error::Io),
    )
}
