use rusterisk_api::{SoundFormat, SoundId};
use rusterisk_media::Frame;
use rusterisk_server::storage::{PlaybackMedia, StorageWorker, WorkerError};
use rusterisk_storage::{Recorder, StorageError};
use std::path::Path;

fn sound(
    root: &Path,
    language: &str,
    name: &str,
    rate: u32,
    sample: i16,
) -> Result<(), WorkerError> {
    let staging = tempfile::tempdir()?;
    let mut recording = Recorder::create(staging.path(), "source", rate)?;
    recording
        .write(Frame::new(&vec![sample; rate as usize / 50], rate).map_err(StorageError::from)?)?;
    let path = root
        .join("sounds")
        .join(language)
        .join(format!("{name}.wav"));
    std::fs::create_dir_all(path.parent().ok_or(WorkerError::Invalid)?)?;
    std::fs::rename(recording.finish()?, path)?;
    Ok(())
}

#[tokio::test]
async fn sound_queries_report_only_playable_language_codec_pairs_and_apply_exact_filters(
) -> Result<(), WorkerError> {
    let root = tempfile::tempdir()?;
    sound(root.path(), "en", "digits/one", 8000, 1234)?;
    sound(root.path(), "ja", "digits/one", 48000, 5678)?;
    sound(root.path(), "en", "welcome", 16000, 2222)?;
    std::fs::write(root.path().join("sounds/en/broken.wav"), b"not audio")?;
    std::fs::write(root.path().join("sounds/en/notes.txt"), b"not a sound")?;
    let (port, worker) = StorageWorker::start(root.path().to_owned()).await?;
    let result = async {
        let sounds = port.sounds(None, None).await?;
        assert_eq!(
            sounds
                .iter()
                .map(|sound| sound.id.as_str())
                .collect::<Vec<_>>(),
            ["digits/one", "welcome"]
        );
        assert_eq!(sounds[0].text, None);
        assert_eq!(
            sounds[0].formats,
            [
                SoundFormat {
                    language: "en".into(),
                    format: "slin".into()
                },
                SoundFormat {
                    language: "ja".into(),
                    format: "slin48".into()
                }
            ]
        );
        let selected = port.sounds(Some("ja"), Some("slin48")).await?;
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].formats.len(), 1);
        for (language, format) in [
            (Some("JA"), None),
            (None, Some("wav")),
            (Some("en"), Some("slin48")),
        ] {
            assert!(port.sounds(language, format).await?.is_empty());
        }
        assert_eq!(port.sounds(Some(""), Some("")).await?, sounds);
        assert!(matches!(
            port.sounds(Some(&"a".repeat(65)), None).await,
            Err(WorkerError::Invalid)
        ));
        for (language, expected) in [("en", 1234), ("ja", 5678)] {
            let name =
                SoundId::try_from("digits/one".to_owned()).map_err(|_| WorkerError::Invalid)?;
            let mut source = port
                .play(PlaybackMedia::LocalizedSound {
                    name,
                    language: rusterisk_api::ChannelId::try_from(language.to_owned())
                        .map_err(|_| WorkerError::Invalid)?,
                })
                .await?;
            let frame = source.read().await?.ok_or(WorkerError::Invalid)?;
            assert!(frame.samples().iter().all(|sample| *sample == expected));
        }
        Ok::<(), WorkerError>(())
    }
    .await;
    let cleanup = worker.close().await;
    result.and(cleanup)
}

#[tokio::test]
async fn absent_sound_libraries_return_an_empty_query_without_creating_directories(
) -> Result<(), WorkerError> {
    let root = tempfile::tempdir()?;
    let (port, worker) = StorageWorker::start(root.path().to_owned()).await?;
    let result = port.sounds(None, None).await;
    let cleanup = worker.close().await;
    assert!(result?.is_empty());
    assert!(!root.path().join("sounds").exists());
    cleanup
}

#[tokio::test]
async fn sound_queries_reject_more_than_sixteen_languages_without_truncating_the_result(
) -> Result<(), WorkerError> {
    let root = tempfile::tempdir()?;
    for index in 0..17 {
        std::fs::create_dir_all(root.path().join(format!("sounds/language-{index}")))?;
    }
    let (port, worker) = StorageWorker::start(root.path().to_owned()).await?;
    let result = port.sounds(None, None).await;
    let cleanup = worker.close().await;
    assert!(matches!(
        result,
        Err(WorkerError::Storage(StorageError::Capacity))
    ));
    cleanup
}

#[cfg(unix)]
#[tokio::test]
async fn sound_queries_reject_symbolic_language_directories() -> Result<(), WorkerError> {
    let root = tempfile::tempdir()?;
    let outside = tempfile::tempdir()?;
    std::fs::create_dir(root.path().join("sounds"))?;
    std::os::unix::fs::symlink(outside.path(), root.path().join("sounds/en"))?;
    let (port, worker) = StorageWorker::start(root.path().to_owned()).await?;
    let result = port.sounds(None, None).await;
    let cleanup = worker.close().await;
    assert!(matches!(
        result,
        Err(WorkerError::Storage(StorageError::InvalidName))
    ));
    cleanup
}
