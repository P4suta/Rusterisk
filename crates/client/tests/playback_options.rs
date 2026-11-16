use rusterisk_client::{
    api::{
        ChannelId, MediaUri, Playback, PlaybackMilliseconds, PlaybackOptions, PlaybackState,
        SoundId,
    },
    Client, Error,
};
use std::{collections::BTreeMap, future::IntoFuture};

async fn accepted(
    axum::extract::Path((resource, target)): axum::extract::Path<(String, String)>,
    axum::extract::Query(query): axum::extract::Query<BTreeMap<String, String>>,
) -> Result<axum::Json<Playback>, axum::http::StatusCode> {
    let expected = [
        ("media", "sound:digits/one"),
        ("lang", "ja"),
        ("offsetms", "1250"),
        ("skipms", "750"),
    ];
    if query.len() != expected.len()
        || expected
            .into_iter()
            .any(|(key, value)| query.get(key).map(String::as_str) != Some(value))
    {
        return Err(axum::http::StatusCode::BAD_REQUEST);
    }
    let kind = match resource.as_str() {
        "channels" => "channel",
        "bridges" => "bridge",
        _ => return Err(axum::http::StatusCode::NOT_FOUND),
    };
    Ok(axum::Json(Playback {
        id: ChannelId::try_from("timed".to_owned())
            .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?,
        media_uri: "sound:digits/one".into(),
        next_media_uri: None,
        target_uri: format!("{kind}:{target}"),
        language: "ja".into(),
        state: PlaybackState::Queued,
    }))
}

#[tokio::test]
async fn typed_playback_timing_reaches_both_public_target_routes() -> Result<(), Error> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let app = axum::Router::new().route(
        "/ari/{resource}/{target}/play/timed",
        axum::routing::post(accepted),
    );
    let server = tokio::spawn(axum::serve(listener, app).into_future());
    let operation = async {
        let client = Client::new(&format!("http://{address}"), "app", "password")?;
        let target = ChannelId::try_from("phone".to_owned())?;
        let playback = ChannelId::try_from("timed".to_owned())?;
        let language = ChannelId::try_from("ja".to_owned())?;
        let media = MediaUri::Sound(
            SoundId::try_from("digits/one".to_owned())
                .map_err(|_| Error::Invalid("playback sound fixture"))?,
        );
        let options = PlaybackOptions {
            offset_ms: PlaybackMilliseconds::try_from(1250)
                .map_err(|_| Error::Invalid("playback timing fixture"))?,
            skip_ms: PlaybackMilliseconds::try_from(750)
                .map_err(|_| Error::Invalid("playback timing fixture"))?,
        };
        let channel = client
            .play_channel_with_options(&target, &playback, &media, Some(&language), options)
            .await?;
        let bridge = client
            .play_bridge_with_options(&target, &playback, &media, Some(&language), options)
            .await?;
        if channel.id != playback
            || bridge.id != playback
            || channel.target_uri != "channel:phone"
            || bridge.target_uri != "bridge:phone"
        {
            return Err(Error::Invalid("playback timing response"));
        }
        Ok(())
    }
    .await;
    server.abort();
    let cleanup = match server.await {
        Err(error) if error.is_cancelled() => Ok(()),
        Err(error) => Err(Error::Cleanup(error.to_string())),
        Ok(result) => result.map_err(Error::from),
    };
    match (operation, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(operation), Err(cleanup)) => Err(Error::Cleanup(format!(
            "{operation}; server cleanup failed: {cleanup}"
        ))),
    }
}
