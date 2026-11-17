use rusterisk_client::{
    api::{ChannelId, Sound, SoundId},
    Client, Error,
};
use rusterisk_media::Frame;
use rusterisk_server::storage::{StoragePort, StorageWorker};
use std::path::Path;

fn failure(error: impl std::fmt::Display) -> Error {
    Error::Io(std::io::Error::other(error.to_string()))
}

pub async fn prepare(
    configuration: &Path,
    data: &Path,
) -> Result<(StoragePort, StorageWorker), Error> {
    std::fs::create_dir_all(data)?;
    for root in [data, configuration] {
        super::playback_position::prepare(root)?;
    }
    let (storage, worker) = StorageWorker::start(data.to_owned())
        .await
        .map_err(failure)?;
    let prepared = async {
        for (language, name, sample) in [
            ("en", "digits/rusterisk-one", 1234),
            ("ja", "digits/rusterisk-one", 2345),
            ("en", "rusterisk-welcome", 3456),
        ] {
            let id = ChannelId::try_from(format!("fixture-{language}-{sample}"))?;
            let mut recording = storage.record(id.clone(), 8000).await.map_err(failure)?;
            for _ in 0..3 {
                recording
                    .try_write(Frame::new(&[sample; 160], 8000).map_err(failure)?)
                    .map_err(failure)?;
            }
            recording.finish().await.map_err(failure)?;
            let source = data.join("recordings").join(format!("{id}.wav"));
            for root in [data, configuration] {
                let destination = root
                    .join("sounds")
                    .join(language)
                    .join(format!("{name}.wav"));
                std::fs::create_dir_all(
                    destination
                        .parent()
                        .ok_or(Error::Invalid("sound fixture directory"))?,
                )?;
                std::fs::copy(&source, destination)?;
            }
            storage.delete_recording(id).await.map_err(failure)?;
        }
        Ok::<(), Error>(())
    }
    .await;
    match prepared {
        Ok(()) => Ok((storage, worker)),
        Err(primary) => match worker.close().await {
            Ok(()) => Err(primary),
            Err(cleanup) => Err(Error::Cleanup(format!(
                "{primary}; fixture storage cleanup failed: {cleanup}"
            ))),
        },
    }
}

fn normalized(mut sounds: Vec<Sound>) -> Vec<Sound> {
    sounds.sort_by(|left, right| left.id.cmp(&right.id));
    for sound in &mut sounds {
        sound.formats.sort_by(|left, right| {
            left.language
                .cmp(&right.language)
                .then(left.format.cmp(&right.format))
        });
    }
    sounds
}

fn missing(result: Result<impl Sized, Error>, expected: &str) -> Result<(), Error> {
    match result {
        Err(Error::Api { status, message }) => {
            assert_eq!(status, 404);
            assert_eq!(message.message, expected);
            Ok(())
        }
        Err(error) => Err(error),
        Ok(_) => Err(Error::Invalid("missing sound query unexpectedly succeeded")),
    }
}

pub async fn compare(actual: &Client, reference: &Client) -> Result<(), Error> {
    for (language, format, count) in [
        (None, None, 3),
        (Some("en"), None, 3),
        (Some("ja"), Some("slin"), 1),
        (Some(""), Some(""), 3),
    ] {
        let observed = normalized(actual.sounds(language, format).await?);
        let expected = normalized(reference.sounds(language, format).await?);
        assert_eq!(observed.len(), count);
        assert_eq!(observed, expected);
    }
    let name = SoundId::try_from("digits/rusterisk-one".to_owned()).map_err(failure)?;
    assert_eq!(
        normalized([actual.sound(&name).await?].into_iter().collect()),
        normalized([reference.sound(&name).await?].into_iter().collect())
    );
    for client in [actual, reference] {
        for (language, format) in [
            (Some("EN"), None),
            (None, Some("wav")),
            (None, Some("ulaw")),
        ] {
            missing(
                client.sounds(language, format).await,
                "No sounds found that matched the query",
            )?;
        }
        let name = SoundId::try_from("missing".to_owned()).map_err(failure)?;
        missing(client.sound(&name).await, "Sound not found")?;
    }
    Ok(())
}
