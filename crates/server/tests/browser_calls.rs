#[path = "../examples/browser/assets.rs"]
mod assets;
#[path = "browser_calls/download_tests.rs"]
mod download_tests;
#[path = "browser_calls/driver.rs"]
mod driver;
#[path = "browser_calls/resources.rs"]
mod resources;
#[path = "../../../xtask/src/browser/runtime_inputs.rs"]
mod runtime_inputs;
use driver::{combine, download_recording, BrowserBackend, BrowserRun};
use rusterisk_config::Config;
use rusterisk_media::Frame;
use rusterisk_server::{runtime::Server, ControlError};
use rusterisk_storage::{Recorder, WaveReader};
use std::{
    net::{Ipv4Addr, SocketAddr},
    path::Path,
    time::Duration,
};
use tokio::{net::TcpListener, sync::oneshot, task::JoinHandle};

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}

fn tone(root: &Path, name: &str, frequency: f64) -> Result<std::path::PathBuf, ControlError> {
    let mut recorder = Recorder::create(root, name, 48000).map_err(failure)?;
    for index in 0..50 {
        let mut samples = [0; 960];
        for (offset, sample) in samples.iter_mut().enumerate() {
            *sample = (6000.0
                * (std::f64::consts::TAU * frequency * (index * 960 + offset) as f64 / 48000.0)
                    .sin()) as i16;
        }
        recorder
            .write(Frame::new(&samples, 48000).map_err(failure)?)
            .map_err(failure)?;
    }
    recorder.finish().map_err(failure)
}

fn recording(path: &Path) -> Result<(), ControlError> {
    let mut reader = WaveReader::open(path).map_err(failure)?;
    let mut matching = [0; 2];
    let mut count = 0;
    while let Some(frame) = reader.read_frame().map_err(failure)? {
        count += 1;
        if count > 500 {
            return Err(ControlError::Capacity);
        }
        let power = frame
            .samples()
            .iter()
            .map(|sample| f64::from(*sample).powi(2))
            .sum::<f64>();
        let rms = (power / frame.samples().len() as f64).sqrt();
        for (matches, frequency) in matching.iter_mut().zip([440.0, 660.0]) {
            let mut sine = 0.0;
            let mut cosine = 0.0;
            for (index, sample) in frame.samples().iter().enumerate() {
                let phase =
                    std::f64::consts::TAU * frequency * index as f64 / f64::from(frame.rate());
                sine += f64::from(*sample) * phase.sin();
                cosine += f64::from(*sample) * phase.cos();
            }
            let amplitude = 2.0 * sine.hypot(cosine) / frame.samples().len() as f64;
            if amplitude > 500.0 && amplitude > rms * 0.4 {
                *matches += 1;
            }
        }
    }
    if matching.into_iter().all(|frames| frames >= 5) {
        Ok(())
    } else {
        Err(ControlError::Adapter(
            "browser recording did not contain both known senders".into(),
        ))
    }
}

#[test]
fn selects_only_the_local_and_reference_browser_fixtures() -> Result<(), ControlError> {
    assert_eq!(BrowserBackend::select(None)?, BrowserBackend::Rusterisk);
    assert_eq!(
        BrowserBackend::select(Some("asterisk"))?,
        BrowserBackend::Asterisk
    );
    assert!(BrowserBackend::select(Some("external")).is_err());
    Ok(())
}

#[test]
fn stored_audio_requires_both_known_senders_even_when_one_sender_is_loud(
) -> Result<(), ControlError> {
    let root = tempfile::tempdir().map_err(failure)?;
    for (name, left, right, accepted) in [
        ("silence", 0.0, 0.0, false),
        ("alice", 15000.0, 0.0, false),
        ("bob", 0.0, 15000.0, false),
        ("both", 6000.0, 6000.0, true),
    ] {
        let mut writer = Recorder::create(root.path(), name, 8000).map_err(failure)?;
        for frame in 0..50 {
            let mut samples = [0; 160];
            for (offset, sample) in samples.iter_mut().enumerate() {
                let time = (frame * 160 + offset) as f64 / 8000.0;
                *sample = (left * (std::f64::consts::TAU * 440.0 * time).sin()
                    + right * (std::f64::consts::TAU * 660.0 * time).sin())
                    as i16;
            }
            writer
                .write(Frame::new(&samples, 8000).map_err(failure)?)
                .map_err(failure)?;
        }
        let path = writer.finish().map_err(failure)?;
        assert_eq!(recording(&path).is_ok(), accepted, "{name}");
    }
    Ok(())
}

async fn joined(mut task: JoinHandle<Result<(), ControlError>>) -> Result<(), ControlError> {
    match tokio::time::timeout(Duration::from_secs(6), &mut task).await {
        Ok(result) => result.map_err(failure)?,
        Err(primary) => {
            task.abort();
            match task.await {
                Err(error) if error.is_cancelled() => Err(failure(primary)),
                Err(error) => Err(failure(format!("{primary}; fixture task failed: {error}"))),
                Ok(Err(error)) => Err(failure(format!(
                    "{primary}; fixture cleanup failed: {error}"
                ))),
                Ok(Ok(())) => Err(failure(primary)),
            }
        }
    }
}

async fn browser(
    root: &Path,
    app: axum::Router,
    listener: TcpListener,
    server: SocketAddr,
    backend: BrowserBackend,
) -> Result<(), ControlError> {
    let ui = listener.local_addr().map_err(failure)?;
    let data = tempfile::tempdir().map_err(failure)?;
    let left = tone(data.path(), "alice-microphone", 440.0)?;
    let right = tone(data.path(), "bob-microphone", 660.0)?;
    let recorded = data.path().join("recordings/browser-known.wav");
    let (shutdown, stopped) = oneshot::channel::<()>();
    let serving = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                match stopped.await {
                    Ok(()) | Err(_) => {}
                }
            })
            .await
            .map_err(failure)
    });
    let operation = BrowserRun {
        directory: root,
        ui,
        server,
        backend,
        alice: &left,
        bob: &right,
        recording: &recorded,
    }
    .run()
    .await;
    drop(shutdown);
    combine(operation, joined(serving).await)
}

async fn rusterisk(
    root: &Path,
    app: axum::Router,
    listener: TcpListener,
) -> Result<(), ControlError> {
    let ui = listener.local_addr().map_err(failure)?;
    let mut sip = "[browser]\ntype=transport\nprotocol=ws\nbind=127.0.0.1\n".to_owned();
    for name in ["alice", "bob"] {
        sip.push_str(&format!("[{name}-auth]\ntype=auth\nusername={name}\npassword=test-password\n[{name}]\ntype=aor\nmax_contacts=1\n[{name}-endpoint]\ntype=endpoint\ntransport=browser\nauth={name}-auth\naors={name}\nallow=ulaw\nwebrtc=yes\ndtmf_mode=info\ncontext=internal\n"));
    }
    let mut config = Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        &format!(
            "[general]\nallowed_origins=http://{ui}\n[app]\ntype=user\npassword=test-password\n"
        ),
        &sip,
        "",
        "[internal]\nexten => 100,1,Stasis(calls)\n",
    )
    .map_err(failure)?;
    config.http.bind.set_port(0);
    config.rtp.start = 0;
    config.rtp.end = 0;
    let storage = tempfile::tempdir().map_err(failure)?;
    let server = Server::bind(config, 8, storage.path().to_owned()).await?;
    let address = server.local_addr()?;
    let controller = server.controller();
    let serving = tokio::spawn(server.run());
    let operation = browser(root, app, listener, address, BrowserBackend::Rusterisk).await;
    controller.shutdown();
    combine(operation, joined(serving).await)
}

#[tokio::test]
#[ignore = "requires the bounded browser verification container"]
async fn actual_browser_calls_exchange_known_audio_and_store_both_senders(
) -> Result<(), ControlError> {
    let selected = match std::env::var("RUSTERISK_BROWSER_SERVER") {
        Ok(value) => Some(value),
        Err(std::env::VarError::NotPresent) => None,
        Err(error) => return Err(failure(error)),
    };
    let backend = BrowserBackend::select(selected.as_deref())?;
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/browser");
    let app = assets::router(&root).map_err(failure)?;
    let listener = TcpListener::bind("127.0.0.1:0").await.map_err(failure)?;
    match backend {
        BrowserBackend::Rusterisk => rusterisk(&root, app, listener).await,
        BrowserBackend::Asterisk => {
            browser(
                &root,
                app,
                listener,
                SocketAddr::from((Ipv4Addr::LOCALHOST, 8088)),
                backend,
            )
            .await
        }
    }
}
