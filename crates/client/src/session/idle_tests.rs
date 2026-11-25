use super::{Client, Error, Message};
use futures_util::{SinkExt, StreamExt};
use std::time::Duration;
use tokio::io::DuplexStream;
use tokio_tungstenite::{tungstenite::protocol::Role, WebSocketStream};

async fn count_heartbeats(mut peer: WebSocketStream<DuplexStream>) -> Result<u64, Error> {
    let mut count = 0;
    while let Some(frame) = peer.next().await {
        match frame? {
            Message::Ping(_) => {
                count += 1;
                peer.flush().await?;
            }
            Message::Close(_) => return Ok(count),
            Message::Text(_) | Message::Binary(_) | Message::Pong(_) | Message::Frame(_) => {
                return Err(Error::Invalid("unexpected idle session frame"));
            }
        }
    }
    Err(Error::Closed)
}

async fn parks_in_an_idle_hour(interval: Option<Duration>) -> Result<u64, Error> {
    let client = Client::new("http://127.0.0.1", "app", "test-password")?;
    let (local, remote) = tokio::io::duplex(4096);
    let socket = WebSocketStream::from_raw_socket(local, Role::Client, None).await;
    let peer = WebSocketStream::from_raw_socket(remote, Role::Server, None).await;
    tokio::time::pause();
    let session = super::start(&client, &"app".to_owned().try_into()?, socket);
    let ticking = interval.map(|period| {
        tokio::spawn(async move {
            let mut ticks = tokio::time::interval(period);
            loop {
                ticks.tick().await;
            }
        })
    });
    let measure = async {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let metrics = tokio::runtime::Handle::current().metrics();
        let before = metrics.worker_park_count(0);
        tokio::time::sleep(Duration::from_secs(3600)).await;
        let parks = metrics.worker_park_count(0).saturating_sub(before);
        if let Some(ticking) = ticking {
            ticking.abort();
            assert!(ticking.await.is_err_and(|error| error.is_cancelled()));
        }
        session.shutdown().await?;
        Ok::<_, Error>(parks)
    };
    let (measured, received) = tokio::join!(measure, count_heartbeats(peer));
    let parks = measured?;
    assert_eq!(received?, 180);
    Ok(parks)
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
