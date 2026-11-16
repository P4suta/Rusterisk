#[path = "support/native_server.rs"]
mod native_server;
#[path = "reference/playback_position.rs"]
mod position;

#[tokio::test]
async fn sdk_playback_position_controls_change_the_actual_received_audio(
) -> Result<(), rusterisk_client::Error> {
    use rusterisk_client::Error;
    let root = std::env::temp_dir().join(format!(
        "rusterisk-playback-position-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir(&root)?;
    let result = async {
        position::prepare(&root)?;
        native_server::run(&root, "playback-position", position::compare).await
    }
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
