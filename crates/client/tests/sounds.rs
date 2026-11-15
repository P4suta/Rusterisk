use rusterisk_client::{
    api::{Sound, SoundFormat, SoundId},
    Client, Error,
};
use std::{collections::BTreeMap, future::IntoFuture};

fn sound() -> Sound {
    Sound {
        id: "digits/one".into(),
        text: None,
        formats: [SoundFormat {
            language: "en_GB".into(),
            format: "slin16".into(),
        }]
        .into_iter()
        .collect(),
    }
}

#[tokio::test]
async fn sound_methods_preserve_typed_metadata_filters_and_encoded_relative_names(
) -> Result<(), Error> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let app =
        axum::Router::new()
            .route(
                "/ari/sounds",
                axum::routing::get(
                    |axum::extract::Query(query): axum::extract::Query<
                        BTreeMap<String, String>,
                    >| async move {
                        if !query.is_empty() {
                            assert_eq!(
                                query,
                                [
                                    ("format".into(), "slin16".into()),
                                    ("lang".into(), "en_GB".into())
                                ]
                                .into_iter()
                                .collect()
                            );
                        }
                        axum::Json([sound()])
                    },
                ),
            )
            .route(
                "/ari/sounds/{name}",
                axum::routing::get(
                    |axum::extract::Path(name): axum::extract::Path<String>,
                     uri: axum::http::Uri| async move {
                        assert_eq!(name, "digits/one");
                        assert_eq!(uri.path(), "/ari/sounds/digits%2Fone");
                        axum::Json(sound())
                    },
                ),
            );
    let serving = tokio::spawn(axum::serve(listener, app).into_future());
    let result = async {
        let client = Client::new(&format!("http://{address}"), "app", "password")?;
        for (language, format) in [(Some("en_GB"), Some("slin16")), (None, None)] {
            assert_eq!(client.sounds(language, format).await?, [sound()]);
        }
        let name = SoundId::try_from("digits/one".to_owned())
            .map_err(|_| Error::Invalid("sound fixture"))?;
        assert_eq!(client.sound(&name).await?, sound());
        assert!(matches!(
            client.sounds(Some(&"a".repeat(65)), None).await,
            Err(Error::Capacity)
        ));
        Ok::<(), Error>(())
    }
    .await;
    serving.abort();
    assert!(serving.await.is_err_and(|error| error.is_cancelled()));
    result
}
