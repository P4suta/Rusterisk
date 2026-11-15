#[path = "../examples/browser/assets.rs"]
mod assets;
#[path = "browser_calls/resources.rs"]
mod resources;
use rusterisk_config::Config;
use rusterisk_media::Frame;
use rusterisk_server::{runtime::Server, ControlError, Credentials};
use rusterisk_storage::{Recorder, WaveReader};
use std::{path::Path, time::Duration};
use tokio::{net::TcpListener, task::JoinHandle};

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

#[tokio::test]
#[ignore = "requires the bounded browser verification container"]
async fn actual_browser_calls_exchange_known_audio_and_store_both_senders(
) -> Result<(), ControlError> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/browser");
    let app = assets::router(&root).map_err(failure)?;
    let listener = TcpListener::bind("127.0.0.1:0").await.map_err(failure)?;
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
    let data = tempfile::tempdir().map_err(failure)?;
    let left = tone(data.path(), "alice-microphone", 440.0)?;
    let right = tone(data.path(), "bob-microphone", 660.0)?;
    let server = Server::bind(config, 8, data.path().to_owned()).await?;
    let address = server.local_addr()?;
    let controller = server.controller();
    let mut stopped = controller.shutdown_signal();
    let serving_ui = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                while !*stopped.borrow_and_update() {
                    if stopped.changed().await.is_err() {
                        break;
                    }
                }
            })
            .await
            .map_err(failure)
    });
    let serving = tokio::spawn(server.run());
    let result = async {
        let credentials = Credentials::new("app".into(), "test-password".into())?;
        let mut subscription = controller.subscribe(credentials, None, "calls".into()).await?;
        let mut child = tokio::process::Command::new("node")
            .current_dir(&root)
            .args(["--max-old-space-size=256", "node_modules/@playwright/test/cli.js", "test"])
            .env("RUSTERISK_BROWSER_UI", format!("http://{ui}"))
            .env("RUSTERISK_BROWSER_API", format!("http://{address}/ari/"))
            .env("RUSTERISK_BROWSER_SIGNALING", format!("ws://{address}/ws"))
            .env("RUSTERISK_BROWSER_ALICE_AUDIO", left)
            .env("RUSTERISK_BROWSER_BOB_AUDIO", right)
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(failure)?;
        let operation = tokio::time::timeout(Duration::from_secs(150), async {
            let mut events = 0;
            loop {
                tokio::select! {
                    status = child.wait() => {
                        let status = status.map_err(failure)?;
                        break if status.success() { Ok(()) } else { Err(failure(format!("browser fixture failed: {status}"))) };
                    }
                    event = subscription.events.recv() => {
                        event.ok_or(ControlError::Capacity)?;
                        events += 1;
                        if events > 512 { break Err(ControlError::Capacity); }
                    }
                }
            }
        }).await.map_err(failure).and_then(|result| result);
        if let Err(primary) = operation {
            eprintln!("browser resource counters: {}", resources::snapshot(Path::new("/sys/fs/cgroup")));
            if child.id().is_some() {
                if let Err(cleanup) = child.kill().await {
                    return Err(failure(format!("{primary}; child cleanup failed: {cleanup}")));
                }
            }
            return Err(primary);
        }
        recording(&data.path().join("recordings/browser-known.wav"))
    }.await;
    controller.shutdown();
    let mut failures = Vec::with_capacity(3);
    for outcome in [result, joined(serving_ui).await, joined(serving).await] {
        if let Err(error) = outcome {
            failures.push(error.to_string());
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failure(failures.join("; ")))
    }
}
