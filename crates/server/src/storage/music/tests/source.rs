use super::{finish, fixture_error, resize_wave, settings, verify, wave};
use crate::storage::{PreparedMusic, StoragePort, StorageWorker, WorkerError};
use rusterisk_config::MusicOnHold;
use rusterisk_media::Frame;
use rusterisk_storage::{Recorder, StorageError};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[tokio::test]
async fn a_five_millisecond_track_loops_into_full_frames_without_inserting_silence(
) -> Result<(), StorageError> {
    fixture(
        |root| {
            let path = wave(root, "music/short.wav", 8000, 1234)?;
            resize_wave(&path, 44 + 40 * 2)?;
            settings(&[("Main", "music")])
        },
        |_root, port, catalog| async move {
            let mut source = port
                .play_music(&catalog, "main", None)
                .await
                .map_err(fixture_error)?;
            for _ in 0..8 {
                let frame = source
                    .read()
                    .await
                    .map_err(fixture_error)?
                    .ok_or(StorageError::InvalidWave)?;
                verify(
                    frame.rate() == 48000 && frame.samples().len() == 960,
                    "short music did not fill a complete output frame",
                )?;
                verify(
                    frame.samples().iter().all(|sample| *sample == 1234),
                    "music inserted silence or changed the known audio at a loop boundary",
                )?;
            }
            Ok(())
        },
    )
    .await
}

#[tokio::test]
async fn a_single_sample_track_completes_across_bounded_work_without_stopping_storage_commands(
) -> Result<(), StorageError> {
    fixture(
        |root| {
            let path = wave(root, "music/short.wav", 48000, 1234)?;
            resize_wave(&path, 46)?;
            settings(&[("Main", "music")])
        },
        |_root, port, catalog| async move {
            let mut source = port
                .play_music(&catalog, "Main", None)
                .await
                .map_err(fixture_error)?;
            let (audio, recordings) = tokio::join!(source.read(), port.recordings());
            let frame = audio
                .map_err(fixture_error)?
                .ok_or(StorageError::InvalidWave)?;
            verify(
                frame.rate() == 48000 && frame.samples().len() == 960,
                "single-sample music did not complete its bounded frame assembly",
            )?;
            verify(
                frame.samples().iter().all(|sample| *sample == 1234),
                "single-sample music inserted silence between work batches",
            )?;
            verify(
                recordings.map_err(fixture_error)?.is_empty(),
                "storage commands did not finish during short-track assembly",
            )
        },
    )
    .await
}

#[tokio::test]
async fn alphabetical_tracks_with_different_rates_share_a_frame_and_repeat_in_order(
) -> Result<(), StorageError> {
    fixture(
        |root| {
            let second = wave(root, "music/Bravo.wav", 16000, -2345)?;
            resize_wave(&second, 44 + 240 * 2)?;
            let first = wave(root, "music/alpha.wav", 8000, 1234)?;
            resize_wave(&first, 44 + 40 * 2)?;
            settings(&[("Main", "music")])
        },
        |_root, port, catalog| async move {
            let mut source = port
                .play_music(&catalog, "MAIN", None)
                .await
                .map_err(fixture_error)?;
            for _ in 0..8 {
                let frame = source
                    .read()
                    .await
                    .map_err(fixture_error)?
                    .ok_or(StorageError::InvalidWave)?;
                verify(
                    frame.rate() == 48000 && frame.samples().len() == 960,
                    "music changed the output frame duration across tracks",
                )?;
                verify(
                    frame.samples()[..240].iter().all(|sample| *sample == 1234)
                        && frame.samples()[240..].iter().all(|sample| *sample == -2345),
                    "music changed sorted track order or padded a partial track",
                )?;
            }
            Ok(())
        },
    )
    .await
}

#[tokio::test]
async fn restarting_uses_the_last_consumed_position_and_excludes_prefetched_audio(
) -> Result<(), StorageError> {
    fixture(
        |root| {
            let mut recorder = Recorder::create(root, "source-stage", 8000)?;
            for sample in [1234, 2345, 3456, 4567] {
                recorder.write(Frame::new(&[sample; 160], 8000)?)?;
            }
            std::fs::create_dir(root.join("music"))?;
            std::fs::rename(recorder.finish()?, root.join("music/steps.wav"))?;
            settings(&[("Main", "music")])
        },
        |_root, port, catalog| async move {
            let mut source = port
                .play_music(&catalog, "Main", None)
                .await
                .map_err(fixture_error)?;
            let first = source
                .read()
                .await
                .map_err(fixture_error)?
                .ok_or(StorageError::InvalidWave)?;
            verify(
                first.samples().iter().all(|sample| *sample == 1234),
                "music did not begin at its first known frame",
            )?;
            port.recordings().await.map_err(fixture_error)?;
            let cursor = source.music_cursor().ok_or(StorageError::InvalidState)?;
            drop(source);
            port.recordings().await.map_err(fixture_error)?;
            let mut resumed = port
                .play_music(&catalog, "Main", Some(cursor))
                .await
                .map_err(fixture_error)?;
            for sample in [2345, 3456, 4567, 1234] {
                let frame = resumed
                    .read()
                    .await
                    .map_err(fixture_error)?
                    .ok_or(StorageError::InvalidWave)?;
                verify(
                    frame.samples().len() == 960
                        && frame.samples().iter().all(|value| *value == sample),
                    "restart skipped unconsumed audio or lost its loop cursor",
                )?;
            }
            Ok(())
        },
    )
    .await
}

#[tokio::test]
async fn restarting_restores_the_consumed_offset_inside_a_resampled_native_frame(
) -> Result<(), StorageError> {
    fixture(
        |root| {
            let first = wave(root, "music/alpha.wav", 8000, 1234)?;
            resize_wave(&first, 44 + 120 * 2)?;
            wave(root, "music/Bravo.wav", 16000, 2345)?;
            settings(&[("Main", "music")])
        },
        |_root, port, catalog| async move {
            let mut source = port
                .play_music(&catalog, "Main", None)
                .await
                .map_err(fixture_error)?;
            let first = source
                .read()
                .await
                .map_err(fixture_error)?
                .ok_or(StorageError::InvalidWave)?;
            verify(
                first.rate() == 48000 && first.samples().len() == 960,
                "music did not produce the expected frame before saving its cursor",
            )?;
            verify(
                first.samples()[..720].iter().all(|sample| *sample == 1234)
                    && first.samples()[720..].iter().all(|sample| *sample == 2345),
                "music changed the initial partial-track boundary",
            )?;
            port.recordings().await.map_err(fixture_error)?;
            let cursor = source.music_cursor().ok_or(StorageError::InvalidState)?;
            drop(source);
            port.recordings().await.map_err(fixture_error)?;
            let mut resumed = port
                .play_music(&catalog, "Main", Some(cursor))
                .await
                .map_err(fixture_error)?;
            let next = resumed
                .read()
                .await
                .map_err(fixture_error)?
                .ok_or(StorageError::InvalidWave)?;
            verify(
                next.rate() == 48000 && next.samples().len() == 960,
                "resumed music did not fill the next output frame",
            )?;
            verify(
                next.samples()[..720].iter().all(|sample| *sample == 2345)
                    && next.samples()[720..].iter().all(|sample| *sample == 1234),
                "resumed music lost its consumed offset or skipped prefetched audio",
            )
        },
    )
    .await
}

#[tokio::test]
async fn opening_music_rejects_a_selected_asset_that_changed_after_preparation(
) -> Result<(), StorageError> {
    fixture(
        |root| {
            wave(root, "music/known.wav", 8000, 1234)?;
            settings(&[("Main", "music")])
        },
        |root, port, catalog| async move {
            std::fs::OpenOptions::new()
                .write(true)
                .open(root.join("music/known.wav"))?
                .set_len(46)?;
            verify(
                port.play_music(&catalog, "Main", None).await.is_err(),
                "music accepted an asset whose prepared metadata changed",
            )?;
            verify(
                port.recordings().await.map_err(fixture_error)?.is_empty(),
                "music rejection stopped ordinary storage commands",
            )
        },
    )
    .await
}

#[tokio::test]
async fn an_unknown_class_is_rejected_before_creating_a_music_source() -> Result<(), StorageError> {
    fixture(
        |root| {
            wave(root, "music/known.wav", 8000, 1234)?;
            settings(&[("Main", "music")])
        },
        |_root, port, catalog| async move {
            verify(
                matches!(
                    port.play_music(&catalog, "missing", None).await,
                    Err(WorkerError::Invalid)
                ),
                "music accepted an unconfigured class",
            )
        },
    )
    .await
}

#[tokio::test]
async fn music_shares_the_existing_sixty_four_sources_and_returns_capacity_after_drop(
) -> Result<(), StorageError> {
    fixture(
        |root| {
            wave(root, "music/known.wav", 8000, 1234)?;
            settings(&[("Main", "music")])
        },
        |_root, port, catalog| async move {
            let mut sources = Vec::with_capacity(64);
            for _ in 0..64 {
                sources.push(
                    port.play_music(&catalog, "Main", None)
                        .await
                        .map_err(fixture_error)?,
                );
            }
            verify(
                matches!(
                    port.play_music(&catalog, "Main", None).await,
                    Err(WorkerError::Storage(StorageError::Capacity))
                ),
                "music exceeded the shared source capacity",
            )?;
            drop(sources.pop());
            port.recordings().await.map_err(fixture_error)?;
            let mut replacement = port
                .play_music(&catalog, "Main", None)
                .await
                .map_err(fixture_error)?;
            let frame = replacement
                .read()
                .await
                .map_err(fixture_error)?
                .ok_or(StorageError::InvalidWave)?;
            verify(
                frame.samples().iter().all(|sample| *sample == 1234),
                "released source capacity could not play known music",
            )
        },
    )
    .await
}

async fn fixture<F>(
    setup: impl FnOnce(&Path) -> Result<MusicOnHold, StorageError>,
    check: impl FnOnce(PathBuf, StoragePort, PreparedMusic) -> F,
) -> Result<(), StorageError>
where
    F: Future<Output = Result<(), StorageError>>,
{
    let directory = tempfile::tempdir()?;
    let result = async {
        let classes = setup(directory.path())?;
        let (port, worker) = StorageWorker::start(directory.path().to_owned())
            .await
            .map_err(fixture_error)?;
        let result = async {
            let catalog = port.prepare_music(classes).await.map_err(fixture_error)?;
            tokio::time::timeout(
                Duration::from_secs(5),
                check(directory.path().to_owned(), port, catalog),
            )
            .await
            .map_err(|error| fixture_error(format!("music source exercise: {error}")))?
        }
        .await;
        finish(result, worker.close().await.map_err(fixture_error))
    }
    .await;
    finish(result, directory.close().map_err(StorageError::from))
}
