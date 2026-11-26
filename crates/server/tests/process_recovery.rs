use futures_util::StreamExt;
use rusterisk_api::{Channel, Event, EventKind, StoredRecording};
use rusterisk_media::{AudioFormat, Frame, RtpSender};
use rusterisk_server::{rtp::SocketPair, ControlError};
use rusterisk_test_support::Wait;
use std::{
    net::{Ipv4Addr, SocketAddr},
    path::Path,
    time::Duration,
};
use tokio_tungstenite::{
    tungstenite::{client::IntoClientRequest, Message},
    MaybeTlsStream, WebSocketStream,
};
#[path = "process_recovery/process.rs"]
mod process;
use process::{combine, failure, ChildServer};

type Events = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

async fn request(
    client: &reqwest::Client,
    address: SocketAddr,
    method: reqwest::Method,
    path: &str,
    query: &[(&str, &str)],
) -> Result<reqwest::Response, ControlError> {
    let encoded = serde_urlencoded::to_string(query).map_err(failure)?;
    let url = if encoded.is_empty() {
        format!("http://{address}/ari/{path}")
    } else {
        format!("http://{address}/ari/{path}?{encoded}")
    };
    client
        .request(method, url)
        .basic_auth("app", Some("test-password"))
        .send()
        .await
        .map_err(failure)?
        .error_for_status()
        .map_err(failure)
}

async fn events(address: SocketAddr) -> Result<Events, ControlError> {
    let mut request = format!("ws://{address}/ari/events?app=recovery")
        .into_client_request()
        .map_err(failure)?;
    request.headers_mut().insert(
        "Authorization",
        "Basic YXBwOnRlc3QtcGFzc3dvcmQ=".parse().map_err(failure)?,
    );
    request
        .headers_mut()
        .insert("Sec-WebSocket-Protocol", "ari".parse().map_err(failure)?);
    let (socket, _) = tokio_tungstenite::connect_async(request)
        .await
        .map_err(failure)?;
    Ok(socket)
}

async fn recording_event(
    events: &mut Events,
    name: &str,
    finished: bool,
) -> Result<(), ControlError> {
    Wait::Ready.bounded(async {
        for _ in 0..64 {
            match events.next().await {
                Some(Ok(Message::Text(text))) => {
                    let event: Event = serde_json::from_str(&text)?;
                    let matched = if finished {
                        matches!(event.kind, EventKind::RecordingFinished { recording } if recording.name.as_str() == name)
                    } else {
                        matches!(event.kind, EventKind::RecordingStarted { recording } if recording.name.as_str() == name)
                    };
                    if matched {
                        return Ok(());
                    }
                }
                Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
                Some(Ok(_)) | None => {
                    return Err(ControlError::Adapter("event connection closed".into()))
                }
                Some(Err(error)) => return Err(failure(error)),
            }
        }
        Err(ControlError::Capacity)
    })
    .await
    .map_err(failure)?
}

fn verify_signal(path: &Path) -> Result<(), ControlError> {
    let mut reader = rusterisk_storage::WaveReader::open(path).map_err(failure)?;
    let required = (u64::from(reader.rate()) * 80).div_ceil(1000);
    let mut consecutive = 0_u64;
    for _ in 0..1000 {
        let Some(frame) = reader.read_frame().map_err(failure)? else {
            break;
        };
        for sample in frame.samples() {
            consecutive = if (i32::from(*sample) - 4000).abs() < 300 {
                consecutive + 1
            } else {
                0
            };
            if consecutive >= required {
                return Ok(());
            }
        }
    }
    Err(ControlError::Adapter(
        "recording lacks the received signal".into(),
    ))
}

fn history(root: &Path, channel: &str) -> Result<String, ControlError> {
    let database = rusqlite::Connection::open_with_flags(
        root.join("calls.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .map_err(failure)?;
    database
        .query_row(
            "SELECT outcome FROM calls WHERE channel=?1",
            [channel],
            |row| row.get(0),
        )
        .map_err(failure)
}

#[test]
fn recovery_audio_oracle_rejects_silence_wrong_audio_and_short_transients(
) -> Result<(), ControlError> {
    let root = tempfile::tempdir().map_err(failure)?;
    for (index, (rate, sample, frames, accepted)) in [
        (8000, 0, 4, false),
        (8000, -7000, 4, false),
        (8000, 4000, 3, false),
        (8000, 4000, 4, true),
        (16000, 4000, 4, false),
        (16000, 4000, 8, true),
    ]
    .into_iter()
    .enumerate()
    {
        let mut recorder =
            rusterisk_storage::Recorder::create(root.path(), &format!("oracle-{index}"), rate)
                .map_err(failure)?;
        for _ in 0..frames {
            recorder
                .write(Frame::new(&[sample; 160], rate).map_err(failure)?)
                .map_err(failure)?;
        }
        let file = recorder.finish().map_err(failure)?;
        assert_eq!(verify_signal(&file).is_ok(), accepted);
    }
    Ok(())
}

#[tokio::test]
async fn forced_process_exit_recovers_audio_and_history_through_the_next_server_start(
) -> Result<(), ControlError> {
    recovery(None, Path::new(env!("CARGO_BIN_EXE_rusterisk"))).await
}

async fn recovery(previous: Option<&Path>, next: &Path) -> Result<(), ControlError> {
    let root = tempfile::tempdir().map_err(failure)?;
    let settings = root.path().join("config");
    let data = root.path().join("data");
    std::fs::create_dir(&settings).map_err(failure)?;
    for (name, source) in [
        ("http.conf", "[general]\nbindaddr=127.0.0.1\nbindport=0\n"),
        ("ari.conf", "[app]\ntype=user\npassword=test-password\n"),
        ("pjsip.conf", ""),
        ("rtp.conf", ""),
        (
            "extensions.conf",
            "[installed]\nexten => 707,1,Stasis(recovery)\n",
        ),
    ] {
        std::fs::write(settings.join(name), source).map_err(failure)?;
    }
    let peer = SocketPair::bind(Ipv4Addr::LOCALHOST.into(), 0, 0).map_err(failure)?;
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Wait::Io.duration())
        .build()
        .map_err(failure)?;
    let log = root.path().join("first.log");
    let mut first = match previous {
        Some(binary) => ChildServer::start_from(binary, &settings, &data, log).await?,
        None => ChildServer::start(&settings, &data, log).await?,
    };
    let first_operation = Wait::Scenario
        .bounded(async {
            let mut events = events(first.address()).await?;
            let remote = peer.local_addr().map_err(failure)?.to_string();
            let mut active = None;
            for id in ["completed-call", "interrupted-call"] {
                let channel: Channel = request(
                    &client,
                    first.address(),
                    reqwest::Method::POST,
                    "channels/externalMedia",
                    &[
                        ("app", "recovery"),
                        ("channelId", id),
                        ("external_host", &remote),
                        ("format", "ulaw"),
                        ("transport", "udp"),
                        ("encapsulation", "rtp"),
                    ],
                )
                .await?
                .json()
                .await
                .map_err(failure)?;
                if id == "completed-call" {
                    request(
                        &client,
                        first.address(),
                        reqwest::Method::DELETE,
                        &format!("channels/{id}"),
                        &[],
                    )
                    .await?;
                } else {
                    active = Some(channel);
                }
            }
            let channel = active.ok_or(ControlError::Capacity)?;
            let variables = channel.channelvars.ok_or(ControlError::Capacity)?;
            let ip = variables
                .get("UNICASTRTP_LOCAL_ADDRESS")
                .ok_or(ControlError::Capacity)?;
            let port = variables
                .get("UNICASTRTP_LOCAL_PORT")
                .ok_or(ControlError::Capacity)?;
            let destination: SocketAddr = format!("{ip}:{port}").parse().map_err(failure)?;
            let mut sender = RtpSender::new(AudioFormat::Pcmu, 0, [31; 10]).map_err(failure)?;
            for name in ["complete", "interrupted"] {
                request(
                    &client,
                    first.address(),
                    reqwest::Method::POST,
                    "channels/interrupted-call/record",
                    &[("name", name), ("format", "wav")],
                )
                .await?;
                recording_event(&mut events, name, false).await?;
                let mut tick = tokio::time::interval(Duration::from_millis(20));
                tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                for _ in 0..30 {
                    tick.tick().await;
                    let bytes = sender
                        .packet(Frame::new(&[4000; 160], 8000).map_err(failure)?)
                        .map_err(failure)?
                        .encode()
                        .map_err(failure)?;
                    peer.data()
                        .send_to(&bytes, destination)
                        .await
                        .map_err(failure)?;
                }
                if name == "complete" {
                    request(
                        &client,
                        first.address(),
                        reqwest::Method::POST,
                        "recordings/live/complete/stop",
                        &[],
                    )
                    .await?;
                    recording_event(&mut events, name, true).await?;
                    verify_signal(&data.join("recordings/complete.wav"))?;
                }
            }
            if history(&data, "completed-call")? != "completed"
                || history(&data, "interrupted-call")? != "active"
            {
                return Err(ControlError::Adapter(
                    "pre-crash call history is incorrect".into(),
                ));
            }
            let partial = data.join("recordings/interrupted.partial");
            let mut changes =
                rusterisk_test_support::DirectoryChanges::new(&partial).map_err(failure)?;
            Wait::Ready
                .bounded(async {
                    loop {
                        if std::fs::metadata(&partial).map_err(failure)?.len() >= 44 + 1280 {
                            return Ok::<(), ControlError>(());
                        }
                        changes.changed().await.map_err(failure)?;
                    }
                })
                .await
                .map_err(failure)??;
            let status = first.terminate().await?;
            if status.success() {
                return Err(ControlError::Adapter(
                    "server exited without forced termination".into(),
                ));
            }
            let db = rusqlite::Connection::open(data.join("calls.sqlite3")).map_err(failure)?;
            if previous.is_some() {
                let version: u32 = db.pragma_query_value(None, "user_version", |row| row.get(0)).map_err(failure)?;
                assert_eq!(version, 2);
                return Ok(());
            }
            let writing: u32 = db.query_row(
                "SELECT count(*) FROM recording_operations WHERE name='interrupted' AND operation='create' AND phase='writing'",
                [], |row| row.get(0),
            ).map_err(failure)?;
            assert_eq!(writing, 1);
            Ok(())
        })
        .await
        .map_err(failure)
        .and_then(|result| result);
    let first_cleanup = first.terminate().await.map(|_| ());
    combine(first_operation, first_cleanup)?;
    let audio = std::fs::read(data.join("recordings/complete.wav")).map_err(failure)?;
    if previous.is_some() {
        let backup = root.path().join("backup");
        std::fs::create_dir(&backup).map_err(failure)?;
        copy_tree(&settings, &backup.join("config"))?;
        copy_tree(&data, &backup.join("data"))?;
        verify_install(next, root.path(), &client, &audio).await?;
        let restored = root.path().join("restored");
        copy_tree(&backup, &restored)?;
        verify_install(next, &restored, &client, &audio).await
    } else {
        verify_install(next, root.path(), &client, &audio).await
    }
}

fn copy_tree(source: &Path, target: &Path) -> Result<(), ControlError> {
    std::fs::create_dir(target).map_err(failure)?;
    for entry in std::fs::read_dir(source).map_err(failure)? {
        let entry = entry.map_err(failure)?;
        let destination = target.join(entry.file_name());
        let kind = entry.file_type().map_err(failure)?;
        if kind.is_dir() {
            copy_tree(&entry.path(), &destination)?;
        } else if kind.is_file() {
            std::fs::copy(entry.path(), destination).map_err(failure)?;
        } else {
            return Err(ControlError::Invalid("Unexpected backup entry"));
        }
    }
    Ok(())
}

async fn verify_install(
    binary: &Path,
    root: &Path,
    client: &reqwest::Client,
    audio: &[u8],
) -> Result<(), ControlError> {
    let data = root.join("data");
    let mut restarted = ChildServer::start_from(
        binary,
        &root.join("config"),
        &data,
        root.join("restarted.log"),
    )
    .await?;
    let recovered = async {
        if history(&data, "completed-call")? != "completed"
            || history(&data, "interrupted-call")? != "interrupted"
        {
            return Err(ControlError::Adapter(
                "server restart did not recover call history".into(),
            ));
        }
        let channels: Vec<Channel> = request(
            client,
            restarted.address(),
            reqwest::Method::GET,
            "channels",
            &[],
        )
        .await?
        .json()
        .await
        .map_err(failure)?;
        if !channels.is_empty() {
            return Err(ControlError::Adapter("restart restored live calls".into()));
        }
        let recordings: Vec<StoredRecording> = request(
            client,
            restarted.address(),
            reqwest::Method::GET,
            "recordings/stored",
            &[],
        )
        .await?
        .json()
        .await
        .map_err(failure)?;
        if recordings.len() != 2
            || !["complete", "interrupted"].iter().all(|name| {
                recordings
                    .iter()
                    .any(|recording| recording.name.as_str() == *name)
            })
        {
            return Err(ControlError::Adapter(
                "restart did not publish recovered recordings".into(),
            ));
        }
        for name in ["complete", "interrupted"] {
            verify_signal(&data.join(format!("recordings/{name}.wav")))?;
        }
        let downloaded = request(
            client,
            restarted.address(),
            reqwest::Method::GET,
            "recordings/stored/complete/file",
            &[],
        )
        .await?
        .bytes()
        .await
        .map_err(failure)?;
        assert_eq!(downloaded.as_ref(), audio);
        let route: serde_json::Value = client
            .get(format!(
                "http://{}/rusterisk/diagnostics/extensions/installed/707",
                restarted.address()
            ))
            .basic_auth("app", Some("test-password"))
            .send()
            .await
            .map_err(failure)?
            .error_for_status()
            .map_err(failure)?
            .json()
            .await
            .map_err(failure)?;
        assert_eq!(route["application"], "recovery");
        if data.join("recordings/interrupted.partial").exists() {
            return Err(ControlError::Adapter(
                "partial recording survived recovery".into(),
            ));
        }
        Ok(())
    }
    .await;
    combine(recovered, restarted.terminate().await.map(|_| ()))
}

#[tokio::test]
async fn an_unavailable_distribution_binary_cannot_start_a_process_check(
) -> Result<(), ControlError> {
    let root = tempfile::tempdir().map_err(failure)?;
    let missing = root.path().join("missing-server");
    assert!(ChildServer::start_from(
        &missing,
        root.path(),
        &root.path().join("data"),
        root.path().join("server.log")
    )
    .await
    .is_err());
    root.close().map_err(failure)
}

#[tokio::test]
#[ignore = "Requires the previous and candidate server executables."]
async fn the_candidate_upgrades_old_recordings_and_restores_the_stopped_backup(
) -> Result<(), ControlError> {
    let previous = std::env::var_os("RUSTERISK_BASELINE_SERVER")
        .map(std::path::PathBuf::from)
        .ok_or(ControlError::Invalid(
            "The previous server binary is required",
        ))?;
    let next = std::env::var_os("RUSTERISK_CANDIDATE_SERVER")
        .map(std::path::PathBuf::from)
        .ok_or(ControlError::Invalid(
            "The candidate server binary is required",
        ))?;
    recovery(Some(&previous), &next).await
}
