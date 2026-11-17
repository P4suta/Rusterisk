use super::{
    order_tracks, prepare, prepare_observed, reparse_attributes, MusicCatalog, MusicInventory,
    MusicPreparationError, PreparationCheckpoint, PreparedMusic,
};
use crate::storage::{StorageWorker, WorkerError};
use rusterisk_config::{Config, MusicOnHold};
use rusterisk_media::Frame;
use rusterisk_storage::{Recorder, StorageError, WaveReader};
use std::fs::OpenOptions;
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

mod source;

#[test]
fn preparation_owns_a_sorted_complete_catalog_with_matching_metadata_and_audio(
) -> Result<(), StorageError> {
    fixture(|root| {
        wave(root, "music/main/zulu.wav", 8000, -2345)?;
        wave(root, "music/main/alpha.wav", 48000, 1234)?;
        wave(root, "music/main/Bravo.wav", 16000, 3456)?;
        wave(root, "music/other/音楽.wav", 24000, 4567)?;
        wave(root, "music/main/.hidden.wav", 8000, 5678)?;
        wave(root, "music/main/subdirectory/ignored.wav", 8000, 6789)?;
        std::fs::write(root.join("music/main/notes.txt"), b"not audio")?;
        let catalog = prepared(
            root,
            settings(&[("Main", "music/main"), ("Other", "music/other")])?,
        )?;
        verify(
            catalog.root == std::fs::canonicalize(root)?,
            "catalog lost its data root",
        )?;
        verify(
            catalog.classes.len() == 2,
            "catalog lost a configured class",
        )?;
        let main = &catalog.classes[0];
        verify(
            main.definition.name().as_str() == "Main",
            "catalog changed class spelling",
        )?;
        verify(
            main.tracks
                .iter()
                .map(|track| track.name.as_str())
                .collect::<Vec<_>>()
                == ["alpha.wav", "Bravo.wav", "zulu.wav"],
            "catalog changed the alphabetical track order or included an ignored entry",
        )?;
        let mut total = 0u64;
        for (track, rate, sample) in [
            (&main.tracks[0], 48000, 1234),
            (&main.tracks[1], 16000, 3456),
            (&main.tracks[2], 8000, -2345),
            (&catalog.classes[1].tracks[0], 24000, 4567),
        ] {
            let directory = if track.name == "音楽.wav" {
                "music/other"
            } else {
                "music/main"
            };
            let mut path = catalog.root.clone();
            for component in directory.split('/') {
                path.push(component);
            }
            path.push(&track.name);
            verify(track.rate == rate, "catalog changed the track sample rate")?;
            verify(
                track.samples == u64::from(rate / 50),
                "catalog changed the track sample count",
            )?;
            let encoded_length = std::fs::metadata(&path)?.len();
            verify(
                track.encoded_length == encoded_length,
                "catalog changed the encoded length",
            )?;
            total = total
                .checked_add(encoded_length)
                .ok_or(StorageError::Capacity)?;
            let mut reader = WaveReader::open(&path)?;
            let frame = reader.read_frame()?.ok_or(StorageError::InvalidWave)?;
            verify(
                frame.samples().iter().all(|value| *value == sample),
                "catalog associated a track with different known audio",
            )?;
            verify(
                reader.read_frame()?.is_none(),
                "fixture exceeded its declared audio",
            )?;
        }
        verify(
            catalog_encoded_length(&catalog)? == total,
            "catalog total omitted encoded bytes",
        )?;
        Ok(())
    })
}

#[test]
fn empty_configuration_needs_no_existing_root_and_performs_no_filesystem_preparation(
) -> Result<(), StorageError> {
    fixture(|holder| {
        let missing = holder.join("missing/data");
        let catalog = prepare_observed(
            &missing,
            settings(&[])?,
            || Err(fixture_error("empty settings invoked filesystem service")),
            |_| {
                Err(fixture_error(
                    "empty settings emitted a preparation checkpoint",
                ))
            },
        )
        .map_err(fixture_error)?;
        verify(
            matches!(catalog.catalog, MusicCatalog::Empty),
            "empty settings retained a filesystem inventory",
        )?;
        verify(
            !holder.join("missing").exists() && std::fs::read_dir(holder)?.next().is_none(),
            "empty preparation created or resolved its missing data root",
        )
    })
}

#[test]
fn every_configured_class_must_pass_before_any_catalog_is_returned() -> Result<(), StorageError> {
    fixture(|root| {
        wave(root, "music/first/good.wav", 8000, 1234)?;
        std::fs::create_dir_all(root.join("music/second"))?;
        std::fs::write(root.join("music/second/bad.wav"), b"invalid audio")?;
        let classes = settings(&[("first", "music/first"), ("second", "music/second")])?;
        rejected(prepare(root, classes.clone(), || Ok(())), 6, "wave")?;
        std::fs::remove_file(root.join("music/second/bad.wav"))?;
        wave(root, "music/second/repaired.wav", 8000, -2345)?;
        let catalog = prepared(root, classes)?;
        verify(
            catalog.classes.len() == 2,
            "repaired preparation lost a class",
        )?;
        verify(
            catalog.classes[0].tracks.len() == 1 && catalog.classes[1].tracks.len() == 1,
            "failed preparation leaked partial tracks into its replacement",
        )
    })
}

#[test]
fn missing_empty_nonregular_and_invalid_wave_assets_keep_the_configuration_position(
) -> Result<(), StorageError> {
    fixture(|root| {
        let classes = settings(&[("default", "music")])?;
        rejected(prepare(root, classes.clone(), || Ok(())), 3, "directory")?;
        std::fs::create_dir(root.join("music"))?;
        rejected(prepare(root, classes.clone(), || Ok(())), 3, "empty")?;
        std::fs::create_dir(root.join("music/not-regular.wav"))?;
        rejected(prepare(root, classes.clone(), || Ok(())), 3, "regular")?;
        std::fs::remove_dir(root.join("music/not-regular.wav"))?;
        let path = wave(root, "music/empty.wav", 8000, 1234)?;
        resize_wave(&path, 44)?;
        rejected(prepare(root, classes.clone(), || Ok(())), 3, "empty")?;
        std::fs::remove_file(path)?;
        let path = wave(root, "music/truncated.wav", 8000, 1234)?;
        OpenOptions::new().write(true).open(&path)?.set_len(46)?;
        rejected(prepare(root, classes.clone(), || Ok(())), 3, "wave")?;
        std::fs::remove_file(path)?;
        let path = wave(root, "music/non-pcm.wav", 8000, 1234)?;
        let mut file = OpenOptions::new().write(true).open(path)?;
        file.seek(SeekFrom::Start(20))?;
        file.write_all(&3u16.to_le_bytes())?;
        drop(file);
        rejected(prepare(root, classes, || Ok(())), 3, "wave")
    })
}

#[test]
fn ordering_rejects_case_ambiguous_names_independently_of_host_case_sensitivity(
) -> Result<(), StorageError> {
    fixture(|root| {
        wave(root, "music/first.wav", 8000, 1234)?;
        wave(root, "music/second.wav", 8000, -2345)?;
        let mut catalog = prepared(root, settings(&[("default", "music")])?)?;
        let mut class = catalog.classes.remove(0);
        class.tracks[1].name = "FIRST.wav".into();
        rejected(
            order_tracks(&class.definition, &mut class.tracks),
            3,
            "case",
        )
    })
}

#[test]
fn a_class_accepts_sixty_four_wavs_and_rejects_the_sixty_fifth() -> Result<(), StorageError> {
    fixture(|root| {
        for index in 0..64 {
            wave(root, &format!("music/track-{index:02}.wav"), 8000, 1234)?;
        }
        let classes = settings(&[("default", "music")])?;
        let catalog = prepared(root, classes.clone())?;
        verify(
            catalog.classes[0].tracks.len() == 64,
            "class truncated its exact track capacity",
        )?;
        wave(root, "music/overflow.wav", 8000, -2345)?;
        rejected(prepare(root, classes, || Ok(())), 3, "64")
    })
}

#[test]
fn total_track_capacity_counts_repeated_class_references() -> Result<(), StorageError> {
    fixture(|root| {
        for index in 0..64 {
            wave(root, &format!("music/track-{index:02}.wav"), 8000, 1234)?;
        }
        let references = [
            ("first", "music"),
            ("second", "music"),
            ("third", "music"),
            ("fourth", "music"),
        ];
        let catalog = prepared(root, settings(&references)?)?;
        verify(
            catalog
                .classes
                .iter()
                .map(|class| class.tracks.len())
                .sum::<usize>()
                == 256,
            "total capacity deduplicated class references",
        )?;
        let classes = settings(&[
            ("first", "music"),
            ("second", "music"),
            ("third", "music"),
            ("fourth", "music"),
            ("fifth", "music"),
        ])?;
        rejected(prepare(root, classes, || Ok(())), 15, "256")
    })
}

#[test]
fn entry_capacity_counts_hidden_and_non_audio_names_before_filtering() -> Result<(), StorageError> {
    fixture(|root| {
        wave(root, "music/source.wav", 8000, 1234)?;
        for index in 0..511 {
            std::fs::write(root.join(format!("music/.ignored-{index}.txt")), [])?;
        }
        let classes = settings(&[("default", "music")])?;
        let catalog = prepared(root, classes.clone())?;
        verify(
            catalog.classes[0].tracks.len() == 1,
            "entry budget changed the filtered inventory",
        )?;
        std::fs::write(root.join("music/overflow.txt"), [])?;
        rejected(prepare(root, classes, || Ok(())), 3, "512")
    })
}

#[test]
fn encoded_file_capacity_includes_the_header_and_accepts_its_exact_boundary(
) -> Result<(), StorageError> {
    fixture(|root| {
        let path = wave(root, "music/source.wav", 8000, 1234)?;
        let maximum = 16 * 1024 * 1024;
        resize_wave(&path, maximum)?;
        let classes = settings(&[("default", "music")])?;
        let catalog = prepared(root, classes.clone())?;
        verify(
            catalog.classes[0].tracks[0].encoded_length == maximum,
            "encoded file capacity omitted the header",
        )?;
        resize_wave(&path, maximum + 2)?;
        rejected(prepare(root, classes, || Ok(())), 3, "16 MiB")
    })
}

#[test]
fn encoded_total_capacity_counts_every_class_reference_and_includes_headers(
) -> Result<(), StorageError> {
    fixture(|root| {
        let path = wave(root, "music/source.wav", 8000, 1234)?;
        resize_wave(&path, 16 * 1024 * 1024)?;
        let references = [
            ("first", "music"),
            ("second", "music"),
            ("third", "music"),
            ("fourth", "music"),
        ];
        let catalog = prepared(root, settings(&references)?)?;
        verify(
            catalog_encoded_length(&catalog)? == 64 * 1024 * 1024,
            "encoded total deduplicated class references",
        )?;
        let classes = settings(&[
            ("first", "music"),
            ("second", "music"),
            ("third", "music"),
            ("fourth", "music"),
            ("fifth", "music"),
        ])?;
        rejected(prepare(root, classes, || Ok(())), 15, "64 MiB")
    })
}

#[test]
fn truncating_a_validated_track_is_rejected_before_catalog_publication() -> Result<(), StorageError>
{
    fixture(|root| {
        let path = wave(root, "music/source.wav", 8000, 1234)?;
        let original_length = std::fs::metadata(&path)?.len();
        let mut mutation = None;
        let result = prepare_observed(
            root,
            settings(&[("default", "music")])?,
            || Ok(()),
            |checkpoint| {
                match checkpoint {
                    PreparationCheckpoint::TrackValidated { class, name }
                        if class.as_str() == "default" && name == "source.wav" =>
                    {
                        mutate_once(&mut mutation, || {
                            OpenOptions::new().write(true).open(&path)?.set_len(46)?;
                            verify(
                                std::fs::metadata(&path)?.len() != original_length,
                                "truncate fixture did not alter observable metadata",
                            )
                        });
                    }
                    PreparationCheckpoint::TrackValidated { .. }
                    | PreparationCheckpoint::BeforePublication => {}
                }
                Ok(())
            },
        );
        mutation_succeeded(mutation)?;
        rejected(result, 3, "changed")
    })
}

#[test]
fn replacing_a_validated_track_is_rejected_before_catalog_publication() -> Result<(), StorageError>
{
    fixture(|root| {
        let source = wave(root, "music/source.wav", 8000, 1234)?;
        let original_length = std::fs::metadata(&source)?.len();
        let replacement = wave(root, "retained/replacement.wav", 8000, -2345)?;
        resize_wave(&replacement, 684)?;
        let mut mutation = None;
        let result = prepare_observed(
            root,
            settings(&[("default", "music")])?,
            || Ok(()),
            |checkpoint| {
                match checkpoint {
                    PreparationCheckpoint::TrackValidated { class, name }
                        if class.as_str() == "default" && name == "source.wav" =>
                    {
                        mutate_once(&mut mutation, || {
                            std::fs::rename(&source, root.join("retained/original.wav"))?;
                            std::fs::rename(&replacement, &source)?;
                            verify(
                                std::fs::metadata(&source)?.len() != original_length,
                                "replacement fixture did not alter observable metadata",
                            )
                        });
                    }
                    PreparationCheckpoint::TrackValidated { .. }
                    | PreparationCheckpoint::BeforePublication => {}
                }
                Ok(())
            },
        );
        mutation_succeeded(mutation)?;
        rejected(result, 3, "changed")
    })
}

#[test]
fn replacing_the_root_with_unchanged_audio_is_rejected_before_publication(
) -> Result<(), StorageError> {
    fixture(|holder| {
        let root = holder.join("data");
        let source = wave(&root, "music/source.wav", 8000, 1234)?;
        let file_metadata = std::fs::metadata(&source)?;
        let class_modified = set_modified(&root.join("music"), fixed_time(1_600_000_002)?)?;
        let root_modified = set_modified(&root, fixed_time(1_600_000_000)?)?;
        let retired = holder.join("retired-root");
        let mut mutation = None;
        let result = prepare_observed(
            &root,
            settings(&[("default", "music")])?,
            || Ok(()),
            |checkpoint| {
                if let PreparationCheckpoint::BeforePublication = checkpoint {
                    mutate_once(&mut mutation, || {
                        std::fs::rename(&root, &retired)?;
                        std::fs::create_dir(&root)?;
                        std::fs::rename(retired.join("music"), root.join("music"))?;
                        set_modified(&root.join("music"), class_modified)?;
                        let replacement_modified = set_modified(&root, fixed_time(1_700_000_000)?)?;
                        verify(
                            replacement_modified != root_modified,
                            "root replacement fixture did not alter observable metadata",
                        )?;
                        unchanged_audio_metadata(&source, &file_metadata)
                    });
                }
                Ok(())
            },
        );
        mutation_succeeded(mutation)?;
        rejected(result, 3, "changed")
    })
}

#[test]
fn replacing_an_ancestor_with_unchanged_audio_is_rejected_before_publication(
) -> Result<(), StorageError> {
    fixture(|holder| {
        let root = holder.join("data");
        let source = wave(&root, "music/ancestor/library/source.wav", 8000, 1234)?;
        let file_metadata = std::fs::metadata(&source)?;
        let library_modified = set_modified(
            &root.join("music/ancestor/library"),
            fixed_time(1_600_000_006)?,
        )?;
        let ancestor_modified =
            set_modified(&root.join("music/ancestor"), fixed_time(1_600_000_004)?)?;
        let parent_modified = set_modified(&root.join("music"), fixed_time(1_600_000_002)?)?;
        let root_modified = set_modified(&root, fixed_time(1_600_000_000)?)?;
        let retired = holder.join("retired-ancestor");
        let mut mutation = None;
        let result = prepare_observed(
            &root,
            settings(&[("default", "music/ancestor/library")])?,
            || Ok(()),
            |checkpoint| {
                if let PreparationCheckpoint::BeforePublication = checkpoint {
                    mutate_once(&mut mutation, || {
                        std::fs::rename(root.join("music/ancestor"), &retired)?;
                        std::fs::create_dir(root.join("music/ancestor"))?;
                        std::fs::rename(
                            retired.join("library"),
                            root.join("music/ancestor/library"),
                        )?;
                        set_modified(&root.join("music/ancestor/library"), library_modified)?;
                        let replacement_modified =
                            set_modified(&root.join("music/ancestor"), fixed_time(1_700_000_000)?)?;
                        set_modified(&root.join("music"), parent_modified)?;
                        set_modified(&root, root_modified)?;
                        verify(
                            replacement_modified != ancestor_modified,
                            "ancestor replacement fixture did not alter observable metadata",
                        )?;
                        verify(
                            std::fs::metadata(&root)?.modified()? == root_modified
                                && std::fs::metadata(root.join("music"))?.modified()?
                                    == parent_modified,
                            "ancestor fixture changed an unaffected parent timestamp",
                        )?;
                        unchanged_audio_metadata(&source, &file_metadata)
                    });
                }
                Ok(())
            },
        );
        mutation_succeeded(mutation)?;
        rejected(result, 3, "changed")
    })
}

#[test]
fn cancellation_never_publishes_a_partial_catalog() -> Result<(), StorageError> {
    fixture(|root| {
        wave(root, "music/first.wav", 8000, 1234)?;
        wave(root, "music/second.wav", 8000, -2345)?;
        let classes = settings(&[("default", "music")])?;
        let result = prepare(root, classes.clone(), || {
            Err(StorageError::Io(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "fixture cancelled music preparation",
            )))
        });
        verify(result.is_err(), "cancelled preparation returned a catalog")?;
        let catalog = prepared(root, classes)?;
        verify(
            catalog.classes[0].tracks.len() == 2,
            "cancellation left stale partial tracks",
        )
    })
}

#[cfg(any(unix, windows))]
#[test]
fn preparation_rejects_linked_roots_ancestors_and_wave_candidates() -> Result<(), StorageError> {
    fixture(|root| {
        fixture(|outside| {
            wave(outside, "music/source.wav", 8000, 1234)?;
            directory_link(outside, &root.join("linked-root"))?;
            rejected(
                prepare(
                    &root.join("linked-root"),
                    settings(&[("default", "music")])?,
                    || Ok(()),
                ),
                3,
                "link",
            )?;
            std::fs::create_dir(root.join("music"))?;
            directory_link(&outside.join("music"), &root.join("music/linked"))?;
            rejected(
                prepare(root, settings(&[("default", "music/linked")])?, || Ok(())),
                3,
                "link",
            )?;
            std::fs::create_dir(root.join("candidates"))?;
            file_link(
                &outside.join("music/source.wav"),
                &root.join("candidates/linked.wav"),
            )?;
            rejected(
                prepare(root, settings(&[("default", "candidates")])?, || Ok(())),
                3,
                "link",
            )?;
            verify(
                outside.join("music/source.wav").is_file(),
                "rejected preparation changed external assets",
            )
        })
    })
}

#[test]
fn reparse_detection_rejects_the_attribute_without_requiring_other_flags(
) -> Result<(), StorageError> {
    for attributes in [0x0000_0400, 0x0000_0401, 0x0000_0410, 0xffff_ffff] {
        verify(
            reparse_attributes(attributes),
            "reparse attribute was accepted",
        )?;
    }
    for attributes in [0, 1, 0x10, 0x200, 0x1000] {
        verify(
            !reparse_attributes(attributes),
            "ordinary attributes were rejected as a reparse point",
        )?;
    }
    Ok(())
}

#[tokio::test]
async fn failed_preparation_preserves_worker_service_and_releases_every_owned_handle(
) -> Result<(), StorageError> {
    let directory = tempfile::tempdir()?;
    let setup = (|| {
        std::fs::create_dir(directory.path().join("music"))?;
        std::fs::write(directory.path().join("music/bad.wav"), b"not audio")?;
        settings(&[("default", "music")])
    })();
    let classes = match setup {
        Ok(classes) => classes,
        Err(error) => {
            return finish(Err(error), directory.close().map_err(StorageError::from));
        }
    };
    let started = StorageWorker::start(directory.path().to_owned()).await;
    let (port, worker) = match started {
        Ok(started) => started,
        Err(error) => {
            return finish(
                Err(fixture_error(error)),
                directory.close().map_err(StorageError::from),
            )
        }
    };
    let result = async {
        rejected(port.prepare_music(classes.clone()).await, 3, "wave")?;
        verify(
            port.recordings().await.map_err(fixture_error)?.is_empty(),
            "music failure stopped ordinary storage service",
        )?;
        std::fs::remove_file(directory.path().join("music/bad.wav"))?;
        wave(directory.path(), "music/repaired.wav", 8000, 1234)?;
        let catalog = port
            .prepare_music(classes.clone())
            .await
            .map_err(fixture_error)?;
        let catalog = inventory(catalog)?;
        verify(
            catalog.classes[0].tracks.len() == 1,
            "worker retained a failed preparation",
        )
    }
    .await;
    let result = finish(result, worker.close().await.map_err(fixture_error));
    let closed = port.prepare_music(classes).await;
    let result = finish(
        result,
        verify(
            matches!(
                closed,
                Err(MusicPreparationError::Worker(WorkerError::Closed))
            ),
            "closed worker accepted music preparation",
        ),
    );
    finish(result, directory.close().map_err(StorageError::from))
}

fn prepared(root: &Path, classes: MusicOnHold) -> Result<MusicInventory, StorageError> {
    inventory(prepare(root, classes, || Ok(())).map_err(fixture_error)?)
}

fn catalog_encoded_length(catalog: &MusicInventory) -> Result<u64, StorageError> {
    catalog
        .classes
        .iter()
        .flat_map(|class| &class.tracks)
        .try_fold(0u64, |length, track| {
            length
                .checked_add(track.encoded_length)
                .ok_or(StorageError::Capacity)
        })
}

fn inventory(catalog: PreparedMusic) -> Result<MusicInventory, StorageError> {
    match catalog.catalog {
        MusicCatalog::Files(inventory) => Ok(inventory),
        MusicCatalog::Empty => Err(fixture_error("expected a nonempty filesystem inventory")),
    }
}

fn settings(classes: &[(&str, &str)]) -> Result<MusicOnHold, StorageError> {
    let mut source = String::new();
    for (name, directory) in classes {
        source.push_str(&format!("[{name}]\nmode=files\ndirectory={directory}\n"));
    }
    let config = Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=test-password\n",
        "",
        "",
        "",
    )
    .and_then(|config| config.with_music_on_hold_configuration(&source))
    .map_err(fixture_error)?;
    Ok(config.music_on_hold)
}

fn wave(root: &Path, relative: &str, rate: u32, sample: i16) -> Result<PathBuf, StorageError> {
    let mut recorder = Recorder::create(root, "asset-stage", rate)?;
    recorder.write(Frame::new(&[sample; 960][..(rate / 50) as usize], rate)?)?;
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().ok_or(StorageError::InvalidName)?)?;
    std::fs::rename(recorder.finish()?, &path)?;
    Ok(path)
}

fn resize_wave(path: &Path, length: u64) -> Result<(), StorageError> {
    let payload = u32::try_from(length.checked_sub(44).ok_or(StorageError::InvalidWave)?)
        .map_err(|_| StorageError::Capacity)?;
    let riff = u32::try_from(length.checked_sub(8).ok_or(StorageError::InvalidWave)?)
        .map_err(|_| StorageError::Capacity)?;
    let mut file = OpenOptions::new().write(true).open(path)?;
    file.set_len(length)?;
    file.seek(SeekFrom::Start(4))?;
    file.write_all(&riff.to_le_bytes())?;
    file.seek(SeekFrom::Start(40))?;
    file.write_all(&payload.to_le_bytes())?;
    Ok(())
}

fn mutate_once(
    receipt: &mut Option<Result<(), StorageError>>,
    mutate: impl FnOnce() -> Result<(), StorageError>,
) {
    *receipt = Some(match receipt.take() {
        None => mutate(),
        Some(Ok(())) => Err(fixture_error("preparation checkpoint repeated a mutation")),
        Some(Err(error)) => Err(fixture_error(format!(
            "{error}; preparation checkpoint repeated a mutation"
        ))),
    });
}

fn mutation_succeeded(receipt: Option<Result<(), StorageError>>) -> Result<(), StorageError> {
    receipt.ok_or_else(|| fixture_error("expected preparation checkpoint was absent"))?
}

fn unchanged_audio_metadata(path: &Path, expected: &std::fs::Metadata) -> Result<(), StorageError> {
    let current = std::fs::metadata(path)?;
    verify(
        current.len() == expected.len() && current.modified()? == expected.modified()?,
        "directory replacement fixture changed the audio metadata",
    )
}

fn fixed_time(seconds: u64) -> Result<SystemTime, StorageError> {
    SystemTime::UNIX_EPOCH
        .checked_add(Duration::from_secs(seconds))
        .ok_or(StorageError::Capacity)
}

fn set_modified(path: &Path, time: SystemTime) -> Result<SystemTime, StorageError> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_READ_ATTRIBUTES: u32 = 0x0080;
        const FILE_WRITE_ATTRIBUTES: u32 = 0x0100;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        options
            .access_mode(FILE_READ_ATTRIBUTES | FILE_WRITE_ATTRIBUTES)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS);
    }
    let file = options.open(path)?;
    file.set_modified(time)?;
    Ok(file.metadata()?.modified()?)
}

fn rejected<T>(
    result: Result<T, MusicPreparationError>,
    line: usize,
    reason: &str,
) -> Result<(), StorageError> {
    match result {
        Err(MusicPreparationError::Configuration(error)) => verify(
            error.file == "musiconhold.conf" && error.line == line && error.reason.contains(reason),
            format!("asset failure lost its source position or reason: {error}"),
        ),
        Err(error) => Err(fixture_error(format!(
            "expected positioned asset rejection, received {error}"
        ))),
        Ok(_) => Err(fixture_error(format!(
            "expected musiconhold.conf:{line} rejection containing {reason}"
        ))),
    }
}

fn verify(condition: bool, message: impl Into<String>) -> Result<(), StorageError> {
    if condition {
        Ok(())
    } else {
        Err(fixture_error(message.into()))
    }
}

fn fixture_error(error: impl std::fmt::Display) -> StorageError {
    StorageError::Io(std::io::Error::other(error.to_string()))
}

fn finish(
    result: Result<(), StorageError>,
    cleanup: Result<(), StorageError>,
) -> Result<(), StorageError> {
    match (result, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(error), Err(cleanup)) => Err(fixture_error(format!(
            "{error}; music fixture cleanup failed: {cleanup}"
        ))),
    }
}

fn fixture(check: impl FnOnce(&Path) -> Result<(), StorageError>) -> Result<(), StorageError> {
    let directory = tempfile::tempdir()?;
    let result = check(directory.path());
    finish(result, directory.close().map_err(StorageError::from))
}

#[cfg(unix)]
fn directory_link(source: &Path, destination: &Path) -> Result<(), StorageError> {
    std::os::unix::fs::symlink(source, destination).map_err(StorageError::from)
}

#[cfg(windows)]
fn directory_link(source: &Path, destination: &Path) -> Result<(), StorageError> {
    std::os::windows::fs::symlink_dir(source, destination).map_err(StorageError::from)
}

#[cfg(unix)]
fn file_link(source: &Path, destination: &Path) -> Result<(), StorageError> {
    std::os::unix::fs::symlink(source, destination).map_err(StorageError::from)
}

#[cfg(windows)]
fn file_link(source: &Path, destination: &Path) -> Result<(), StorageError> {
    std::os::windows::fs::symlink_file(source, destination).map_err(StorageError::from)
}
