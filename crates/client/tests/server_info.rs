use rusterisk_client::{
    api::{AsteriskInfo, InfoSection, StatusInfo, SystemInfo},
    Client, Error,
};
use std::{collections::BTreeMap, future::IntoFuture};

#[tokio::test]
async fn server_information_uses_the_public_route_and_comma_separated_typed_sections(
) -> Result<(), Error> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let app =
        axum::Router::new().route(
            "/ari/asterisk/info",
            axum::routing::get(
                |axum::extract::Query(parameters): axum::extract::Query<
                    BTreeMap<String, String>,
                >| async move {
                    if let Some(only) = parameters.get("only") {
                        assert_eq!(only, "system,status");
                        assert_eq!(parameters.len(), 1);
                    } else {
                        assert!(parameters.is_empty());
                    }
                    axum::Json(AsteriskInfo {
                        system: Some(SystemInfo {
                            version: "fixture".into(),
                            entity_id: "instance".into(),
                        }),
                        status: Some(StatusInfo {
                            startup_time: "2026-11-16T00:00:00Z".into(),
                            last_reload_time: "2026-11-16T00:00:00Z".into(),
                        }),
                        ..AsteriskInfo::default()
                    })
                },
            ),
        );
    let server = tokio::spawn(axum::serve(listener, app).into_future());
    let result = async {
        let client = Client::new(&format!("http://{address}"), "app", "password")?;
        for only in [&[InfoSection::System, InfoSection::Status][..], &[]] {
            let info = client.server_info(only).await?;
            assert!(info.build.is_none());
            assert!(info.config.is_none());
            assert_eq!(
                info.system
                    .ok_or(Error::Invalid("missing system information"))?
                    .version,
                "fixture"
            );
            assert!(info.status.is_some());
        }
        Ok(())
    }
    .await;
    server.abort();
    assert!(server.await.is_err_and(|error| error.is_cancelled()));
    result
}

#[tokio::test]
async fn server_information_refuses_unbounded_section_lists_before_network_io() -> Result<(), Error>
{
    let client = Client::new("http://127.0.0.1:9", "app", "password")?;
    assert!(matches!(
        client.server_info(&[InfoSection::Build; 5]).await,
        Err(Error::Capacity)
    ));
    Ok(())
}
