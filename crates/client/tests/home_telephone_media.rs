#[path = "support/native_server.rs"]
mod native_server;
#[path = "home_telephone_media/scenario.rs"]
mod scenario;

#[tokio::test]
async fn gateway_media_without_a_subprotocol_offer_exchanges_received_audio(
) -> Result<(), rusterisk_client::Error> {
    use rusterisk_client::Error;
    let root = std::env::temp_dir().join(format!(
        "rusterisk-home-telephone-media-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir(&root)?;
    let result = native_server::run(
        &root,
        "home-telephone-fixture",
        async |_client, _session, _application, _callback, address| {
            scenario::compare(address).await
        },
    )
    .await;
    match (result, std::fs::remove_dir_all(root)) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(error)) => Err(Error::from(error)),
        (Err(primary), Err(cleanup)) => Err(Error::Cleanup(format!(
            "{primary}; fixture cleanup failed: {cleanup}"
        ))),
    }
}
