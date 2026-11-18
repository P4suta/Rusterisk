use super::{failure, recording, resources, runtime_inputs};
use rusterisk_client::{
    api::{ChannelId, EventFilter, EventFilterRequest, EventKind, RecordingState},
    Client, Session,
};
use rusterisk_server::ControlError;
use std::{
    io::{self, Write},
    net::{Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};
use tokio::io::AsyncWrite;

const MAX_RECORDING_BYTES: usize = 1024 * 1024;
const RECORDING_NAME: &str = "browser-known";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum BrowserBackend {
    Rusterisk,
    PackagedRusterisk,
    Asterisk,
    PackagedAsterisk,
}

impl BrowserBackend {
    pub(super) fn select(value: Option<&str>) -> Result<Self, ControlError> {
        match value {
            None | Some("rusterisk") => Ok(Self::Rusterisk),
            Some("packaged-rusterisk") => Ok(Self::PackagedRusterisk),
            Some("asterisk") => Ok(Self::Asterisk),
            Some("packaged-asterisk") => Ok(Self::PackagedAsterisk),
            Some(_) => Err(failure("unknown browser call fixture")),
        }
    }

    const fn name(self) -> &'static str {
        match self {
            Self::Rusterisk | Self::PackagedRusterisk => "rusterisk",
            Self::Asterisk | Self::PackagedAsterisk => "asterisk",
        }
    }

    pub(super) fn ui_bind(self) -> SocketAddr {
        let port = match self {
            Self::Rusterisk | Self::Asterisk => 0,
            Self::PackagedRusterisk | Self::PackagedAsterisk => 3000,
        };
        SocketAddr::from((Ipv4Addr::LOCALHOST, port))
    }

    pub(super) fn external_server(self) -> Option<SocketAddr> {
        match self {
            Self::Rusterisk => None,
            Self::PackagedRusterisk | Self::Asterisk | Self::PackagedAsterisk => {
                Some(SocketAddr::from((Ipv4Addr::LOCALHOST, 8088)))
            }
        }
    }
}

struct AudioBuffer(Vec<u8>);

impl AsyncWrite for AudioBuffer {
    fn poll_write(
        mut self: Pin<&mut Self>,
        _context: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        if bytes.len() > MAX_RECORDING_BYTES - self.0.len() {
            return Poll::Ready(Err(io::Error::other(
                "browser recording exceeds one mebibyte",
            )));
        }
        self.0.extend_from_slice(bytes);
        Poll::Ready(Ok(bytes.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

pub(super) fn combine(
    operation: Result<(), ControlError>,
    cleanup: Result<(), ControlError>,
) -> Result<(), ControlError> {
    match (operation, cleanup) {
        (operation, Ok(())) => operation,
        (Ok(()), Err(error)) => Err(error),
        (Err(operation), Err(cleanup)) => Err(failure(format!(
            "{operation}; browser fixture cleanup failed: {cleanup}"
        ))),
    }
}

pub(super) async fn download_recording(
    client: &Client,
    destination: &Path,
) -> Result<(), ControlError> {
    let mut audio = AudioBuffer(Vec::new());
    let name = ChannelId::try_from(RECORDING_NAME.to_owned()).map_err(failure)?;
    tokio::time::timeout(
        Duration::from_secs(10),
        client.download_stored_recording(&name, &mut audio),
    )
    .await
    .map_err(failure)?
    .map_err(failure)?;
    let mut file = std::fs::File::create_new(destination).map_err(failure)?;
    let written = file.write_all(&audio.0).and_then(|()| file.flush());
    drop(file);
    let checked = written
        .map_err(failure)
        .and_then(|()| recording(destination));
    match checked {
        Ok(()) => Ok(()),
        Err(error) => combine(
            Err(error),
            std::fs::remove_file(destination).map_err(failure),
        ),
    }
}

pub(super) fn recording_complete(event: &EventKind) -> Result<bool, ControlError> {
    if let EventKind::RecordingFinished { recording } = event {
        return if recording.name != RECORDING_NAME {
            Ok(false)
        } else if recording.state == RecordingState::Done && recording.format == "wav" {
            Ok(true)
        } else {
            Err(failure(
                "browser recording ended without a stored WAVE file",
            ))
        };
    }
    if matches!(event, EventKind::RecordingFailed { recording } if recording.name == RECORDING_NAME)
    {
        return Err(failure("browser recording failed before download"));
    }
    Ok(false)
}

pub(super) struct BrowserRun<'a> {
    pub directory: &'a Path,
    pub ui: SocketAddr,
    pub server: SocketAddr,
    pub backend: BrowserBackend,
    pub alice: &'a Path,
    pub bob: &'a Path,
    pub recording: &'a Path,
}

impl BrowserRun<'_> {
    pub async fn run(self) -> Result<(), ControlError> {
        let client = Client::new(&format!("http://{}", self.server), "app", "test-password")
            .map_err(failure)?;
        let application = ChannelId::try_from("calls".to_owned()).map_err(failure)?;
        let mut subscription = client.subscribe(&application).await.map_err(failure)?;
        let operation = async {
            let filter = EventFilterRequest {
                allowed: Some(
                    ["StasisStart", "RecordingFinished", "RecordingFailed"]
                        .into_iter()
                        .map(|name| EventFilter {
                            event_type: name.into(),
                        })
                        .collect(),
                ),
                disallowed: None,
            };
            client
                .filter_application(application.as_str(), &filter)
                .await
                .map_err(failure)?;
            self.run_subscribed(&client, &mut subscription).await
        }
        .await;
        combine(operation, subscription.shutdown().await.map_err(failure))
    }

    async fn run_subscribed(
        &self,
        client: &Client,
        subscription: &mut Session,
    ) -> Result<(), ControlError> {
        let mut child = tokio::process::Command::new("node")
            .current_dir(self.directory)
            .args([
                "--max-old-space-size=256",
                "node_modules/@playwright/test/cli.js",
                "test",
            ])
            .env("RUSTERISK_BROWSER_UI", format!("http://{}", self.ui))
            .env("RUSTERISK_BROWSER_SERVER", self.backend.name())
            .env(
                "RUSTERISK_BROWSER_API",
                format!("http://{}/ari/", self.server),
            )
            .env(
                "RUSTERISK_BROWSER_SIGNALING",
                format!("ws://{}/ws", self.server),
            )
            .env("RUSTERISK_BROWSER_ALICE_AUDIO", self.alice)
            .env("RUSTERISK_BROWSER_BOB_AUDIO", self.bob)
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(failure)?;
        let operation = tokio::time::timeout(Duration::from_secs(150), async {
            let mut events = 0;
            let mut browser_done = false;
            let mut recording_done = false;
            while !(browser_done && recording_done) {
                tokio::select! {
                    status = child.wait(), if !browser_done => {
                        let status = status.map_err(failure)?;
                        if !status.success() {
                            return Err(failure(format!("browser fixture failed: {status}")));
                        }
                        browser_done = true;
                    }
                    event = subscription.next_event() => {
                        recording_done |= recording_complete(&event.map_err(failure)?.kind)?;
                        events += 1;
                        if events > 512 {
                            return Err(ControlError::Capacity);
                        }
                    }
                }
            }
            Ok(())
        })
        .await
        .map_err(failure)
        .and_then(|result| result);
        if let Err(primary) = operation {
            eprintln!(
                "browser resource counters: {}",
                resources::snapshot(Path::new("/sys/fs/cgroup"))
            );
            if child.id().is_some() {
                if let Err(cleanup) = child.kill().await {
                    return Err(failure(format!(
                        "{primary}; child cleanup failed: {cleanup}"
                    )));
                }
            }
            return Err(primary);
        }
        download_recording(client, self.recording).await?;
        if let Some(artifacts) = std::env::var_os("RUSTERISK_BROWSER_ARTIFACTS") {
            runtime_inputs::RuntimeInputs::new(
                self.ui,
                self.server,
                self.alice,
                self.bob,
                self.recording,
            )
            .and_then(|inputs| inputs.save(&PathBuf::from(artifacts).join("runtime")))
            .map_err(failure)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod selection_tests {
    use super::BrowserBackend;
    use rusterisk_server::ControlError;
    use std::net::{Ipv4Addr, SocketAddr};

    #[test]
    fn packaged_rusterisk_joins_the_owned_server_with_an_exact_browser_origin(
    ) -> Result<(), ControlError> {
        for (selected, ui_port, server_port, route) in [
            (None, 0, None, "rusterisk"),
            (Some("rusterisk"), 0, None, "rusterisk"),
            (Some("asterisk"), 0, Some(8088), "asterisk"),
            (Some("packaged-rusterisk"), 3000, Some(8088), "rusterisk"),
            (Some("packaged-asterisk"), 3000, Some(8088), "asterisk"),
        ] {
            let backend = BrowserBackend::select(selected)?;
            assert_eq!(
                backend.ui_bind(),
                SocketAddr::from((Ipv4Addr::LOCALHOST, ui_port)),
                "{selected:?}"
            );
            assert_eq!(
                backend.external_server(),
                server_port.map(|port| SocketAddr::from((Ipv4Addr::LOCALHOST, port))),
                "{selected:?}"
            );
            assert_eq!(backend.name(), route, "{selected:?}");
        }
        assert!(BrowserBackend::select(Some("external")).is_err());
        Ok(())
    }
}
