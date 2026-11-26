use axum::response::IntoResponse;
use rusterisk_client::{api, Client, Error};
use std::future::IntoFuture;

#[tokio::test]
async fn live_recording_queries_and_controls_preserve_methods_states_and_errors(
) -> Result<(), Error> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let (observed, mut operations) = tokio::sync::mpsc::channel(6);
    let app = axum::Router::new().fallback(move |request:axum::extract::Request| {
        let observed = observed.clone();
        async move {
            let path = request.uri().path();
            if path == "/ari/recordings/live/missing" {
                return (axum::http::StatusCode::NOT_FOUND, axum::Json(api::ApiError {message:"Recording not found".into()})).into_response();
            }
            if request.method() == axum::http::Method::GET {
                return axum::Json(serde_json::json!({"name":"known", "format":"wav", "state":"recording", "target_uri":"bridge:mix"})).into_response();
            }
            assert!(request.uri().query().is_none_or(str::is_empty));
            match observed.try_send((request.method().to_string(), path.to_owned())) {
                Ok(()) => axum::http::StatusCode::NO_CONTENT.into_response(),
                Err(_) => axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response(),
            }
        }
    });
    let serving = tokio::spawn(axum::serve(listener, app).into_future());
    let result = async {
        let client = Client::new(&format!("http://{address}"), "app", "password")?;
        let name = rusterisk_client::RecordingName::try_from("known".to_owned())?;
        let recording = client.live_recording(&name).await?;
        assert_eq!(recording.state, api::RecordingState::Recording);
        assert_eq!(recording.target_uri, "bridge:mix");
        assert_eq!(recording.duration, None);
        client.pause_recording(&name).await?;
        client.resume_recording(&name).await?;
        client.mute_recording(&name).await?;
        client.unmute_recording(&name).await?;
        client.stop_recording(&name).await?;
        client.cancel_recording(&name).await?;
        for (method,path) in [("POST","/ari/recordings/live/known/pause"),("DELETE","/ari/recordings/live/known/pause"),("POST","/ari/recordings/live/known/mute"),("DELETE","/ari/recordings/live/known/mute"),("POST","/ari/recordings/live/known/stop"),("DELETE","/ari/recordings/live/known")] {
            assert_eq!(operations.recv().await.ok_or(Error::Closed)?, (method.to_owned(),path.to_owned()));
        }
        let missing = rusterisk_client::RecordingName::try_from("missing".to_owned())?;
        assert!(matches!(client.live_recording(&missing).await, Err(Error::Api {status:404,message}) if message.message == "Recording not found"));
        assert!(matches!(client.cancel_recording(&missing).await, Err(Error::Api {status:404,message}) if message.message == "Recording not found"));
        Ok::<(),Error>(())
    }.await;
    serving.abort();
    assert!(serving.await.is_err_and(|error| error.is_cancelled()));
    result
}
