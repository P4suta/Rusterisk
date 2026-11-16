use rusterisk_client::Error;

#[tokio::test]
async fn sdk_channel_and_bridge_recordings_contain_actual_received_audio() -> Result<(), Error> {
    let root = std::env::temp_dir().join(format!(
        "rusterisk-recording-audio-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir(&root)?;
    let result = super::native_server::run(
        &root,
        "recording-audio",
        async |client, session, application, callback, _address| {
            super::recording::compare(client, session, application, callback).await
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
