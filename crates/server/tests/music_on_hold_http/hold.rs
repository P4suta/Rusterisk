use super::{
    audio, closed, combine, command, configuration, failure, joined, music, request, started,
    Events, Peer,
};
use futures_util::StreamExt;
use reqwest::Method;
use rusterisk_api::{Event, EventKind};
use rusterisk_media::Frame;
use rusterisk_server::{rtp::SocketPair, runtime::Server, ControlError};
use rusterisk_storage::Recorder;
use rusterisk_test_support::Wait;
use std::{net::SocketAddr, path::Path, time::Duration};
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, http::HeaderValue};

#[tokio::test]
async fn hold_plays_the_default_class_and_unhold_restores_received_bridge_voice(
) -> Result<(), ControlError> {
    let directory = tempfile::tempdir().map_err(failure)?;
    let result = run(directory.path()).await;
    combine(result, directory.close().map_err(failure))
}

fn main_music(root: &Path) -> Result<(), ControlError> {
    let mut recorder = Recorder::create(root, "main-music-fixture", 8000).map_err(failure)?;
    for _ in 0..4 {
        recorder
            .write(Frame::new(&[2400; 160], 8000).map_err(failure)?)
            .map_err(failure)?;
    }
    let path = recorder.finish().map_err(failure)?;
    let directory = root.join("main-music");
    std::fs::create_dir(&directory).map_err(failure)?;
    std::fs::rename(path, directory.join("known.wav")).map_err(failure)
}

async fn accepted(
    http: &reqwest::Client,
    address: SocketAddr,
    method: Method,
    path: &str,
    query: &[(&str, &str)],
) -> Result<(), ControlError> {
    let status = request(http, address, method, path, query).await?.status();
    if status != reqwest::StatusCode::NO_CONTENT {
        return Err(failure(format!(
            "Expected HTTP 204, received {}",
            status.as_u16()
        )));
    }
    Ok(())
}

async fn ended(events: &mut Events, expected: &str) -> Result<(), ControlError> {
    Wait::Ready.bounded(async {
        for _ in 0..64 {
            let message = events
                .next()
                .await
                .ok_or(ControlError::Capacity)?
                .map_err(failure)?;
            let event: Event = serde_json::from_str(message.to_text().map_err(failure)?)?;
            if matches!(event.kind, EventKind::StasisEnd { channel } if channel.id.as_str() == expected)
            {
                return Ok(());
            }
        }
        Err(ControlError::Capacity)
    })
    .await
    .map_err(failure)?
}

async fn run(root: &Path) -> Result<(), ControlError> {
    let config_root = root.join("configuration");
    let data = root.join("data");
    std::fs::create_dir(&config_root).map_err(failure)?;
    std::fs::create_dir(&data).map_err(failure)?;
    let classes = "[default]\nmode=files\ndirectory=moh\nsort=alpha\n[Main]\nmode=files\ndirectory=main-music\nsort=alpha\n";
    let config = configuration(&config_root)?;
    std::fs::write(config_root.join("musiconhold.conf"), classes).map_err(failure)?;
    let mut config = config
        .with_music_on_hold_configuration(classes)
        .map_err(failure)?;
    config.http.bind.set_port(0);
    music(&data, Frame::new(&[1200; 160], 8000).map_err(failure)?, 4)?;
    main_music(&data)?;
    let mut left = Peer::new(83)?;
    let mut right = Peer::new(84)?;
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(failure)?;
    let server = Server::bind(config, 4, data).await?;
    let controller = server.controller();
    let address = server.local_addr();
    let server = tokio::spawn(server.run());
    let address = match address {
        Ok(address) => address,
        Err(error) => {
            controller.shutdown();
            return combine(Err(error), joined(server).await);
        }
    };
    let mut events = None;
    let mut phase = "event connection";
    let result = Wait::Ready
        .bounded(async {
            let mut event_request = format!("ws://{address}/ari/events?app=music")
                .into_client_request()
                .map_err(failure)?;
            let mut authorization = HeaderValue::from_static("Basic YXBwOnRlc3QtcGFzc3dvcmQ=");
            authorization.set_sensitive(true);
            event_request
                .headers_mut()
                .insert("Authorization", authorization);
            events = Some(
                tokio_tungstenite::connect_async(event_request)
                    .await
                    .map_err(failure)?
                    .0,
            );
            let events = events.as_mut().ok_or(ControlError::Capacity)?;
            phase = "external channels";
            left.create(&http, address, "root").await?;
            started(events, "root").await?;
            right.create(&http, address, "winner").await?;
            started(events, "winner").await?;
            command(
                &http,
                address,
                Method::POST,
                "bridges/music",
                &[("type", "mixing,proxy_media")],
            )
            .await?;
            for channel in ["root", "winner"] {
                command(
                    &http,
                    address,
                    Method::POST,
                    "bridges/music/addChannel",
                    &[("channel", channel)],
                )
                .await?;
            }
            phase = "initial received bridge voice";
            audio(&mut left, &mut right, [4000, -7000], [-7000, 4000], 4).await?;
            phase = "explicit Main music";
            accepted(
                &http,
                address,
                Method::POST,
                "channels/root/moh",
                &[("mohClass", "Main")],
            )
            .await?;
            audio(&mut left, &mut right, [4000, -7000], [2400, 4000], 12).await?;
            accepted(&http, address, Method::DELETE, "channels/root/moh", &[]).await?;
            phase = "bridge voice after explicit Main music";
            audio(&mut left, &mut right, [5000, -6000], [-6000, 5000], 4).await?;
            phase = "automatic default HOLD music";
            accepted(&http, address, Method::POST, "channels/root/hold", &[]).await?;
            audio(&mut left, &mut right, [4000, -7000], [1200, 0], 12).await?;
            phase = "muted default HOLD music";
            accepted(
                &http,
                address,
                Method::POST,
                "channels/root/mute",
                &[("direction", "out")],
            )
            .await?;
            audio(&mut left, &mut right, [4000, -7000], [0, 0], 4).await?;
            phase = "unmuted default HOLD music";
            accepted(
                &http,
                address,
                Method::DELETE,
                "channels/root/mute",
                &[("direction", "out")],
            )
            .await?;
            audio(&mut left, &mut right, [4000, -7000], [1200, 0], 12).await?;
            phase = "UNHOLD received bridge voice";
            accepted(&http, address, Method::DELETE, "channels/root/hold", &[]).await?;
            audio(&mut left, &mut right, [9000, -5000], [-5000, 9000], 4).await?;
            phase = "HOLD before terminal release";
            accepted(&http, address, Method::POST, "channels/root/hold", &[]).await?;
            audio(&mut left, &mut right, [4000, -7000], [1200, 0], 4).await?;
            phase = "terminal release during HOLD music";
            accepted(&http, address, Method::DELETE, "channels/root", &[]).await?;
            ended(events, "root").await?;
            let status = request(&http, address, Method::GET, "channels/root", &[])
                .await?
                .status();
            if status != reqwest::StatusCode::NOT_FOUND {
                return Err(ControlError::Invalid(
                    "Ended HOLD channel remained available",
                ));
            }
            Ok(())
        })
        .await
        .map_err(failure)
        .and_then(|result| result);
    if result.is_err() {
        eprintln!("Music HOLD boundary failed: phase={phase}");
    }
    let mut result = result;
    for resource in ["channels/root", "channels/winner", "bridges/music"] {
        result = combine(
            result,
            command(&http, address, Method::DELETE, resource, &[]).await,
        );
    }
    if let Some(events) = &mut events {
        result = combine(result, closed(events).await);
    }
    drop(events);
    controller.shutdown();
    result = combine(result, joined(server).await);
    result = combine(
        result,
        tokio::net::TcpListener::bind(address)
            .await
            .map_err(failure)
            .map(drop),
    );
    for address in [left.destination, right.destination].into_iter().flatten() {
        let rebound = address
            .port()
            .checked_add(1)
            .ok_or(ControlError::Capacity)
            .and_then(|end| {
                SocketPair::bind(address.ip(), address.port(), end)
                    .map(drop)
                    .map_err(failure)
            });
        result = combine(result, rebound);
    }
    result
}
