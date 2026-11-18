#[path = "../../../client/tests/support/native_server.rs"]
mod native_server;

use super::{failure, recording};
use rusterisk_client::{
    api::{EventKind, LiveRecording, RecordingState},
    Error,
};
use rusterisk_media::Frame;
use rusterisk_server::ControlError;
use rusterisk_storage::Recorder;
use std::path::{Path, PathBuf};

#[test]
fn only_the_named_finished_recording_allows_the_download() -> Result<(), ControlError> {
    let recording = |name: &str, state| LiveRecording {
        name: name.into(),
        format: "wav".into(),
        state,
        target_uri: "bridge:browser-mix".into(),
        duration: None,
        talking_duration: None,
        silence_duration: None,
        cause: None,
    };
    let finished = |name, state| EventKind::RecordingFinished {
        recording: recording(name, state),
    };
    assert!(super::driver::recording_complete(&finished(
        "browser-known",
        RecordingState::Done
    ))?);
    assert!(!super::driver::recording_complete(&finished(
        "another-recording",
        RecordingState::Done
    ))?);
    assert!(super::driver::recording_complete(&finished(
        "browser-known",
        RecordingState::Canceled
    ))
    .is_err());
    assert!(
        super::driver::recording_complete(&EventKind::RecordingFailed {
            recording: recording("browser-known", RecordingState::Failed),
        })
        .is_err()
    );
    Ok(())
}

fn client_failure(error: impl std::fmt::Display) -> Error {
    Error::Io(std::io::Error::other(error.to_string()))
}

fn saved(root: &Path, both: bool) -> Result<PathBuf, ControlError> {
    let mut writer = Recorder::create(root, "browser-known", 8000).map_err(failure)?;
    for frame in 0..10 {
        let mut samples = [0; 160];
        for (offset, sample) in samples.iter_mut().enumerate() {
            let time = (frame * 160 + offset) as f64 / 8000.0;
            let alice = (std::f64::consts::TAU * 440.0 * time).sin();
            let bob = if both {
                (std::f64::consts::TAU * 660.0 * time).sin()
            } else {
                0.0
            };
            *sample = (6000.0 * (alice + bob)) as i16;
        }
        writer
            .write(Frame::new(&samples, 8000).map_err(failure)?)
            .map_err(failure)?;
    }
    writer.finish().map_err(failure)
}

#[tokio::test]
async fn downloaded_browser_recording_uses_ari_bytes_in_an_owned_directory(
) -> Result<(), ControlError> {
    let source = tempfile::tempdir().map_err(failure)?;
    let destination = tempfile::tempdir().map_err(failure)?;
    let original = saved(source.path(), true)?;
    let expected = std::fs::read(original).map_err(failure)?;
    let downloaded = destination.path().join("browser-known.wav");
    native_server::run(
        source.path(),
        "browser-download",
        async |client, _session, _application, _callback, _address| {
            super::download_recording(client, &downloaded)
                .await
                .map_err(client_failure)?;
            recording(&downloaded).map_err(client_failure)?;
            assert_eq!(std::fs::read(&downloaded)?, expected);
            Ok(())
        },
    )
    .await
    .map_err(failure)
}

#[tokio::test]
async fn rejected_browser_downloads_do_not_publish_missing_or_one_sender_audio(
) -> Result<(), ControlError> {
    for missing in [false, true] {
        let source = tempfile::tempdir().map_err(failure)?;
        let destination = tempfile::tempdir().map_err(failure)?;
        if !missing {
            saved(source.path(), false)?;
        }
        let downloaded = destination.path().join("browser-known.wav");
        native_server::run(
            source.path(),
            "browser-download",
            async |client, _session, _application, _callback, _address| {
                assert!(super::download_recording(client, &downloaded)
                    .await
                    .is_err());
                assert!(!downloaded.exists());
                Ok(())
            },
        )
        .await
        .map_err(failure)?;
    }
    Ok(())
}
