use super::Server;
use crate::ControlError;
use rusterisk_config::Config;
use std::time::Duration;

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}

fn configuration() -> Result<Config, ControlError> {
    let mut config = Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=idle-password\n",
        "[udp]\ntype=transport\nprotocol=udp\nbind=127.0.0.1:0\n",
        "",
        "",
    )
    .map_err(failure)?;
    config.http.bind.set_port(0);
    Ok(config)
}

// The server starts on real time because its storage worker is a thread.
// A paused runtime then parks once for each timer that it has to wait for.
// The hour itself costs a few parks while its timer moves through the wheel levels.
async fn parks_in_an_idle_hour(interval: Option<Duration>) -> Result<u64, ControlError> {
    let data = tempfile::tempdir().map_err(failure)?;
    let server = Server::bind(configuration()?, 4, data.path().to_owned()).await?;
    let controller = server.controller();
    let serving = tokio::spawn(server.run());
    let ticking = interval.map(|period| {
        tokio::spawn(async move {
            let mut ticks = tokio::time::interval(period);
            loop {
                ticks.tick().await;
            }
        })
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    tokio::time::pause();
    tokio::time::sleep(Duration::from_secs(60)).await;
    let metrics = tokio::runtime::Handle::current().metrics();
    let before = metrics.worker_park_count(0);
    let activity = controller.activity.snapshot();
    tokio::time::sleep(Duration::from_secs(3600)).await;
    let parks = metrics.worker_park_count(0).saturating_sub(before);
    assert_eq!(controller.activity.snapshot(), activity);
    if let Some(ticking) = ticking {
        ticking.abort();
    }
    tokio::time::resume();
    controller.shutdown();
    let served = tokio::time::timeout(Duration::from_secs(10), serving)
        .await
        .map_err(failure)?
        .map_err(failure)?;
    data.close().map_err(failure)?;
    served.map(|()| parks)
}

#[tokio::test]
async fn an_idle_server_waits_for_no_timer_in_an_hour() -> Result<(), ControlError> {
    let parks = parks_in_an_idle_hour(None).await?;
    assert!(parks <= 8, "an idle server parked {parks} times in an hour");
    Ok(())
}

#[tokio::test]
async fn the_idle_measure_counts_a_periodic_timer() -> Result<(), ControlError> {
    let parks = parks_in_an_idle_hour(Some(Duration::from_millis(20))).await?;
    assert!(
        parks >= 180_000,
        "a 20 ms interval added {parks} parks in an hour"
    );
    Ok(())
}
