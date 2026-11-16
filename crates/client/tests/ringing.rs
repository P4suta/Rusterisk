use rusterisk_client::{api, Client, Error};
use std::future::IntoFuture;

#[tokio::test]
async fn ringing_methods_use_the_declared_verbs_and_preserve_remote_errors() -> Result<(), Error> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let (sender, mut observed) = tokio::sync::mpsc::channel(2);
    let ringing = axum::routing::on(
        axum::routing::MethodFilter::POST.or(axum::routing::MethodFilter::DELETE),
        move |request: axum::extract::Request| {
            let sender = sender.clone();
            async move {
                let value = (
                    request.method().to_string(),
                    request.uri().path().to_owned(),
                );
                assert!(request.uri().query().is_none_or(str::is_empty));
                assert!(axum::body::to_bytes(request.into_body(), 0).await.is_ok());
                sender
                    .try_send(value)
                    .map_err(|_| axum::http::StatusCode::SERVICE_UNAVAILABLE)?;
                Ok::<_, axum::http::StatusCode>(axum::http::StatusCode::NO_CONTENT)
            }
        },
    );
    let app = axum::Router::new()
        .route("/ari/channels/phone/ring", ringing)
        .route(
            "/ari/channels/missing/ring",
            axum::routing::delete(|| async {
                (
                    axum::http::StatusCode::NOT_FOUND,
                    axum::Json(api::ApiError {
                        message: "Channel not found".into(),
                    }),
                )
            }),
        );
    let server = tokio::spawn(axum::serve(listener, app).into_future());
    let result = async {
        let client = Client::new(&format!("http://{address}"), "app", "password")?;
        let id = api::ChannelId::try_from("phone".to_owned())?;
        client.ring(&id).await?;
        assert_eq!(observed.recv().await.ok_or(Error::Closed)?, ("POST".into(), "/ari/channels/phone/ring".into()));
        client.stop_ringing(&id).await?;
        assert_eq!(observed.recv().await.ok_or(Error::Closed)?, ("DELETE".into(), "/ari/channels/phone/ring".into()));
        assert!(matches!(client.stop_ringing(&api::ChannelId::try_from("missing".to_owned())?).await, Err(Error::Api { status:404, message }) if message.message == "Channel not found"));
        Ok::<(),Error>(())
    }.await;
    server.abort();
    assert!(server.await.is_err_and(|error| error.is_cancelled()));
    result
}
