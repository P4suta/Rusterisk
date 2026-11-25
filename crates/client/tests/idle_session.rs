use rusterisk_client::{Client, Error};
use std::{future::IntoFuture, time::Duration};

// The session connects on real time, and the paused runtime then parks once for each timer wait.
// Each 20 second heartbeat costs about six parks across the session and its server connection.
// The bound of 2160 allows twelve.
async fn parks_in_an_idle_hour(interval: Option<Duration>) -> Result<u64, Error> {
    let config = rusterisk_config::Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=test-password\n",
        "",
        "",
        "",
    )
    .map_err(|_| Error::Invalid("test configuration"))?;
    let (controller, runner, _effects) = rusterisk_server::Controller::new(config, 4);
    let runner = tokio::spawn(runner.run());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(
        axum::serve(listener, rusterisk_server::http::router(controller.clone())).into_future(),
    );
    let result = async {
        let client = Client::new(&format!("http://{address}"), "app", "test-password")?;
        let session = client.subscribe(&"app".to_owned().try_into()?).await?;
        let ticking = interval.map(|period| {
            tokio::spawn(async move {
                let mut ticks = tokio::time::interval(period);
                loop {
                    ticks.tick().await;
                }
            })
        });
        tokio::time::pause();
        tokio::time::sleep(Duration::from_secs(60)).await;
        let metrics = tokio::runtime::Handle::current().metrics();
        let before = metrics.worker_park_count(0);
        tokio::time::sleep(Duration::from_secs(3600)).await;
        let parks = metrics.worker_park_count(0).saturating_sub(before);
        if let Some(ticking) = ticking {
            ticking.abort();
        }
        tokio::time::resume();
        session.shutdown().await?;
        Ok(parks)
    }
    .await;
    controller.shutdown();
    runner
        .await
        .map_err(|_| Error::Closed)?
        .map_err(|_| Error::Closed)?;
    server.abort();
    assert!(server.await.is_err());
    result
}

#[tokio::test]
async fn an_idle_session_wakes_only_for_its_heartbeats_in_an_hour() -> Result<(), Error> {
    let parks = parks_in_an_idle_hour(None).await?;
    assert!(
        parks <= 2160,
        "an idle session parked {parks} times in an hour"
    );
    Ok(())
}

#[tokio::test]
async fn the_idle_measure_counts_a_periodic_timer() -> Result<(), Error> {
    let parks = parks_in_an_idle_hour(Some(Duration::from_millis(20))).await?;
    assert!(
        parks >= 180_000,
        "a 20 ms interval added {parks} parks in an hour"
    );
    Ok(())
}
