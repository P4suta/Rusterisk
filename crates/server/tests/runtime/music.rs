use super::failure;
use rusterisk_config::Config;
use rusterisk_media::Frame;
use rusterisk_server::runtime::Server;
use rusterisk_server::ControlError;
use std::net::SocketAddr;
use std::path::Path;
use std::time::Duration;

#[tokio::test]
async fn startup_rejects_an_invalid_unselected_class_and_releases_listeners_and_storage(
) -> Result<(), ControlError> {
    let directory = tempfile::tempdir().map_err(failure)?;
    let mut bound_addresses = None;
    let result = async {
        wave(directory.path(), "music/default/known.wav")?;
        std::fs::create_dir_all(directory.path().join("music/unused")).map_err(failure)?;
        std::fs::write(directory.path().join("music/unused/bad.wav"), b"invalid audio")
            .map_err(failure)?;
        let (http_reservation, sip_reservation) = reservations()?;
        let http = http_reservation.local_addr().map_err(failure)?;
        let sip = sip_reservation.local_addr().map_err(failure)?;
        let config = configuration(http, sip, "[default]\nmode=files\ndirectory=music/default\n[unused]\nmode=files\ndirectory=music/unused\n")?;
        bound_addresses = Some((http, sip));
        drop(http_reservation);
        drop(sip_reservation);
        match Server::bind(config, 4, directory.path().to_owned()).await {
            Err(error) => verify(error.to_string().contains("musiconhold.conf:6"), format!("startup lost the invalid class position: {error}")),
            Ok(server) => combine(
                Err(failure("startup accepted an invalid unselected music class")),
                stop(server).await,
            ),
        }
    }.await;
    let cleanup =
        bound_addresses.map_or(Ok(()), |(http, sip)| released(http, sip, directory.path()));
    let result = combine(result, cleanup);
    combine(result, directory.close().map_err(failure))
}

#[tokio::test]
async fn startup_with_valid_music_serves_and_releases_every_owned_listener(
) -> Result<(), ControlError> {
    let directory = tempfile::tempdir().map_err(failure)?;
    let mut bound_addresses = None;
    let result = async {
        wave(directory.path(), "music/default/known.wav")?;
        let (http_reservation, sip_reservation) = reservations()?;
        let http = http_reservation.local_addr().map_err(failure)?;
        let sip = sip_reservation.local_addr().map_err(failure)?;
        let config = configuration(
            http,
            sip,
            "[default]\nmode=files\ndirectory=music/default\n",
        )?;
        bound_addresses = Some((http, sip));
        drop(http_reservation);
        drop(sip_reservation);
        let server = Server::bind(config, 4, directory.path().to_owned()).await?;
        let controller = server.controller();
        let exercise = async {
            let result = async {
                let response = reqwest::Client::builder()
                    .no_proxy()
                    .timeout(Duration::from_secs(2))
                    .build()
                    .map_err(failure)?
                    .get(format!("http://{http}/ari/asterisk/ping"))
                    .basic_auth("app", Some("test-password"))
                    .send()
                    .await
                    .map_err(failure)?;
                verify(
                    response.status() == reqwest::StatusCode::OK,
                    "valid music startup did not serve management requests",
                )
            }
            .await;
            controller.shutdown();
            result
        };
        tokio::time::timeout(Duration::from_secs(22), async {
            let (serving, exercised) = tokio::join!(server.run(), exercise);
            combine(exercised, serving)
        })
        .await
        .map_err(failure)?
    }
    .await;
    let cleanup =
        bound_addresses.map_or(Ok(()), |(http, sip)| released(http, sip, directory.path()));
    let result = combine(result, cleanup);
    combine(result, directory.close().map_err(failure))
}

fn reservations() -> Result<(std::net::TcpListener, std::net::UdpSocket), ControlError> {
    let http = std::net::TcpListener::bind("127.0.0.1:0").map_err(failure)?;
    let sip = std::net::UdpSocket::bind("127.0.0.1:0").map_err(failure)?;
    Ok((http, sip))
}

fn configuration(http: SocketAddr, sip: SocketAddr, music: &str) -> Result<Config, ControlError> {
    Config::parse(
        &format!("[general]\nbindaddr=127.0.0.1\nbindport={}\n", http.port()),
        "[app]\ntype=user\npassword=test-password\n",
        &format!("[udp]\ntype=transport\nprotocol=udp\nbind={sip}\n"),
        "",
        "",
    )
    .and_then(|config| config.with_music_on_hold_configuration(music))
    .map_err(failure)
}

fn wave(root: &Path, relative: &str) -> Result<(), ControlError> {
    let mut recorder =
        rusterisk_storage::Recorder::create(root, "music-stage", 8000).map_err(failure)?;
    recorder
        .write(Frame::new(&[1234; 160], 8000).map_err(failure)?)
        .map_err(failure)?;
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().ok_or(ControlError::Capacity)?).map_err(failure)?;
    std::fs::rename(recorder.finish().map_err(failure)?, path).map_err(failure)
}

async fn stop(server: Server) -> Result<(), ControlError> {
    server.controller().shutdown();
    tokio::time::timeout(Duration::from_secs(20), server.run())
        .await
        .map_err(failure)?
}

fn released(http: SocketAddr, sip: SocketAddr, data: &Path) -> Result<(), ControlError> {
    let http = std::net::TcpListener::bind(http).map(drop).map_err(failure);
    let sip = std::net::UdpSocket::bind(sip).map(drop).map_err(failure);
    let store = rusterisk_storage::Store::open(data)
        .map(drop)
        .map_err(failure);
    combine(combine(http, sip), store)
}

fn verify(condition: bool, message: impl std::fmt::Display) -> Result<(), ControlError> {
    if condition {
        Ok(())
    } else {
        Err(failure(message))
    }
}

fn combine(
    primary: Result<(), ControlError>,
    cleanup: Result<(), ControlError>,
) -> Result<(), ControlError> {
    match (primary, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(primary), Err(cleanup)) => Err(failure(format!(
            "{primary}; music startup cleanup failed: {cleanup}"
        ))),
    }
}
