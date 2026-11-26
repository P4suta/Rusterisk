use rusterisk_client::{
    api::{ChannelId, MediaUri, PlaybackState, SoundId},
    Client, Error, Playback,
};
use std::{collections::BTreeMap, future::IntoFuture};

fn playback(target: &str) -> Result<Playback, Error> {
    Ok(Playback {
        id: rusterisk_client::PlaybackId::try_from("announcement".to_owned())?,
        media_uri: "sound:digits/one".into(),
        next_media_uri: None,
        target_uri: target.into(),
        language: "ja".into(),
        state: PlaybackState::Queued,
    })
}

async fn accepted(
    axum::extract::Path((kind, id)): axum::extract::Path<(String, String)>,
    axum::extract::Query(query): axum::extract::Query<BTreeMap<String, String>>,
) -> Result<axum::Json<Playback>, axum::http::StatusCode> {
    assert_eq!(
        query.get("media").map(String::as_str),
        Some("sound:digits/one")
    );
    assert_eq!(query.get("lang").map(String::as_str), Some("ja"));
    assert_eq!(query.len(), 2);
    let target = match kind.as_str() {
        "channels" => format!("channel:{id}"),
        "bridges" => format!("bridge:{id}"),
        _ => return Err(axum::http::StatusCode::NOT_FOUND),
    };
    playback(&target)
        .map(axum::Json)
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)
}

#[tokio::test]
async fn playback_methods_use_public_paths_and_preserve_sound_names_and_languages(
) -> Result<(), Error> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let app = axum::Router::new().route(
        "/ari/{kind}/{id}/play/announcement",
        axum::routing::post(accepted),
    );
    let serving = tokio::spawn(axum::serve(listener, app).into_future());
    let result = async {
        let client = Client::new(&format!("http://{address}"), "app", "password")?;
        let target = ChannelId::try_from("phone".to_owned())?;
        let bridge_id = rusterisk_client::BridgeId::try_from("phone".to_owned())?;
        let id = rusterisk_client::PlaybackId::try_from("announcement".to_owned())?;
        let language = ChannelId::try_from("ja".to_owned())?;
        let media = MediaUri::Sound(
            SoundId::try_from("digits/one".to_owned())
                .map_err(|_| Error::Invalid("sound fixture"))?,
        );
        assert_eq!(
            client
                .play_channel(&target, &id, &media, Some(&language))
                .await?,
            playback("channel:phone")?
        );
        assert_eq!(
            client
                .play_bridge(&bridge_id, &id, &media, Some(&language))
                .await?,
            playback("bridge:phone")?
        );
        Ok(())
    }
    .await;
    serving.abort();
    assert!(serving.await.is_err_and(|error| error.is_cancelled()));
    result
}
