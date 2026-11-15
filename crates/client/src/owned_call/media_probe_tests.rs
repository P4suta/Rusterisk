use super::*;
use axum::{extract::State, response::IntoResponse};
use std::future::IntoFuture;

async fn delayed(
    State(pending): State<mpsc::Sender<oneshot::Sender<()>>>,
) -> axum::response::Response {
    let (release, released) = oneshot::channel();
    if pending.send(release).await.is_err() {
        return axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    let _released = released.await;
    (
        axum::http::StatusCode::SERVICE_UNAVAILABLE,
        axum::Json(api::ApiError {
            message: "late query failure".into(),
        }),
    )
        .into_response()
}

#[tokio::test]
async fn media_notifications_and_stasis_exit_cancel_owned_state_requests_before_the_server_replies(
) -> Result<(), Error> {
    for exiting in [false, true] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let (pending, mut requests) = mpsc::channel(1);
        let (stop, stopped) = oneshot::channel();
        let app = axum::Router::new()
            .route("/ari/channels/{id}/variable", axum::routing::get(delayed))
            .with_state(pending);
        let server = tokio::spawn(
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _stopped = stopped.await;
                })
                .into_future(),
        );
        let result: Result<(), Error> = async {
            let client = Client::new(&format!("http://{address}"), "app", "password")?;
            let (_commands, mut calls) = Calls::new(client, api::ChannelId::try_from("app".to_owned())?);
            let channel: api::Channel = serde_json::from_value(serde_json::json!({"id":"media", "name":"PJSIP/phone", "state":"Up", "accountcode":"", "caller":{"name":"", "number":"100"}, "connected":{"name":"", "number":"200"}, "dialplan":{"context":"default", "exten":"s", "priority":1}, "creationtime":"now", "language":"en"}))?;
            let (progress, received) = watch::channel(CallSnapshot { progress: OwnedCallProgress::new().observe(CallMilestone::Stasis), channel: Some(channel.clone()), failure: None, cleanup: Cleanup::Pending, media_probe_failure: None });
            let (_lifetime, lifetime) = oneshot::channel();
            calls.entries.insert(channel.id.clone(), Entry { progress, lifetime, work: Work::Idle, probe: Probe::Waiting });
            calls.tick();
            let release = tokio::time::timeout(Duration::from_secs(1), requests.recv()).await.map_err(|_| Error::Timeout)?.ok_or(Error::Closed)?;
            let kind = if exiting { api::EventKind::StasisEnd { channel: channel.clone() } }
                else { api::EventKind::ChannelVarset { channel: channel.clone(), variable: "RUSTERISK_MEDIA_CONNECTED".into(), value: "1".into() } };
            calls.event(&api::Event { application: "app".into(), timestamp: "now".into(), kind })?;
            let (id, result) = tokio::time::timeout(Duration::from_secs(1), calls.probes.join_next()).await.map_err(|_| Error::Timeout)?.ok_or(Error::Closed)?.map_err(|_| Error::Closed)?;
            assert!(matches!(result, Err(Error::Closed)));
            calls.probed(id, result);
            assert!(received.borrow().media_probe_failure.is_none());
            assert_eq!(received.borrow().progress.has_reached(CallMilestone::MediaConnected), !exiting);
            if release.send(()).is_err() { tracing::debug!("canceled media request already released its server handler"); }
            calls.event(&api::Event { application: "app".into(), timestamp: "later".into(), kind: api::EventKind::StasisEnd { channel } })?;
            calls.close().await?;
            Ok(())
        }.await;
        if stop.send(()).is_err() {
            return Err(Error::Closed);
        }
        tokio::time::timeout(Duration::from_secs(1), server)
            .await
            .map_err(|_| Error::Timeout)?
            .map_err(|_| Error::Closed)??;
        result?;
    }
    Ok(())
}
