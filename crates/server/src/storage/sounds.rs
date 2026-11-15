use super::{receive, stored::StoredCommand, StoragePort, WorkerError};
use rusterisk_api::{ChannelId, Sound, SoundFormat, SoundId};
use rusterisk_storage::{StorageError, WaveReader};
use std::{
    collections::BTreeMap,
    io,
    path::{Path, PathBuf},
};
use tokio::sync::oneshot;

impl StoragePort {
    pub async fn sounds(
        &self,
        language: Option<&str>,
        format: Option<&str>,
    ) -> Result<Vec<Sound>, WorkerError> {
        for value in [language, format] {
            if value.is_some_and(|value| value.len() > 64 || value.chars().any(char::is_control)) {
                return Err(WorkerError::Invalid);
            }
        }
        let (reply, response) = oneshot::channel();
        self.send_stored(StoredCommand::Sounds {
            language: language
                .filter(|value| !value.is_empty())
                .map(str::to_owned),
            format: format.filter(|value| !value.is_empty()).map(str::to_owned),
            reply,
        })?;
        receive(response).await
    }
}

fn directory(path: &Path) -> Result<(), StorageError> {
    if !std::fs::symlink_metadata(path)?.is_dir() {
        return Err(StorageError::InvalidName);
    }
    Ok(())
}

fn path(root: &Path, name: &SoundId, language: Option<&str>) -> Result<PathBuf, StorageError> {
    let mut path = root.join("sounds");
    directory(&path)?;
    if let Some(language) = language {
        ChannelId::try_from(language.to_owned()).map_err(|_| StorageError::InvalidName)?;
        path.push(language);
        directory(&path)?;
    }
    let mut components = name.as_str().split('/').peekable();
    while let Some(component) = components.next() {
        if components.peek().is_some() {
            path.push(component);
            directory(&path)?;
        } else {
            path.push(format!("{component}.wav"));
        }
    }
    if !std::fs::symlink_metadata(&path)?.is_file() {
        return Err(StorageError::InvalidName);
    }
    Ok(path)
}

fn missing(error: &StorageError) -> bool {
    matches!(error, StorageError::Io(error) if error.kind() == io::ErrorKind::NotFound)
}

pub(super) fn open(
    root: &Path,
    name: &SoundId,
    language: &str,
) -> Result<WaveReader, StorageError> {
    let mut language = language;
    while !language.is_empty() {
        match path(root, name, Some(language)) {
            Ok(path) => return WaveReader::open(&path),
            Err(error) if missing(&error) => {}
            Err(error) => return Err(error),
        }
        language = language.rsplit_once('_').map_or("", |(prefix, _)| prefix);
    }
    match path(root, name, None) {
        Ok(path) => return WaveReader::open(&path),
        Err(error) if missing(&error) => {}
        Err(error) => return Err(error),
    }
    WaveReader::open(&path(root, name, Some("en"))?)
}

fn format(rate: u32) -> Result<&'static str, StorageError> {
    match rate {
        8000 => Ok("slin"),
        12000 => Ok("slin12"),
        16000 => Ok("slin16"),
        24000 => Ok("slin24"),
        32000 => Ok("slin32"),
        44100 => Ok("slin44"),
        48000 => Ok("slin48"),
        _ => Err(StorageError::InvalidWave),
    }
}

struct Scan {
    visited: usize,
    sounds: BTreeMap<String, Sound>,
}

impl Scan {
    fn visit(&mut self) -> Result<(), StorageError> {
        self.visited += 1;
        if self.visited > 4096 {
            return Err(StorageError::Capacity);
        }
        Ok(())
    }

    fn walk(
        &mut self,
        root: &Path,
        language: &str,
        relative: &str,
        depth: u8,
        service: &mut impl FnMut() -> Result<(), StorageError>,
    ) -> Result<(), StorageError> {
        if depth >= 8 {
            return Err(StorageError::Capacity);
        }
        let current = root.join("sounds").join(language).join(relative);
        directory(&current)?;
        for entry in std::fs::read_dir(current)? {
            service()?;
            let entry = entry?;
            self.visit()?;
            let kind = entry.file_type()?;
            if kind.is_symlink() {
                return Err(StorageError::InvalidName);
            }
            let leaf = entry.file_name();
            let Some(leaf) = leaf.to_str() else {
                continue;
            };
            let relative = if relative.is_empty() {
                leaf.to_owned()
            } else {
                format!("{relative}/{leaf}")
            };
            if kind.is_dir() {
                if SoundId::try_from(relative.clone()).is_ok() {
                    self.walk(root, language, &relative, depth + 1, service)?;
                }
            } else if kind.is_file() {
                let Some(name) = relative.strip_suffix(".wav") else {
                    continue;
                };
                let Ok(name) = SoundId::try_from(name.to_owned()) else {
                    continue;
                };
                let reader = match path(root, &name, Some(language))
                    .and_then(|path| WaveReader::open(&path))
                {
                    Ok(reader) => reader,
                    Err(StorageError::InvalidWave) => continue,
                    Err(StorageError::Io(error))
                        if error.kind() == io::ErrorKind::UnexpectedEof =>
                    {
                        continue
                    }
                    Err(error) => return Err(error),
                };
                let codec = format(reader.rate())?;
                if !self.sounds.contains_key(name.as_str()) && self.sounds.len() >= 1024 {
                    return Err(StorageError::Capacity);
                }
                let sound = self
                    .sounds
                    .entry(name.to_string())
                    .or_insert_with(|| Sound {
                        id: name.to_string(),
                        text: None,
                        formats: Vec::new(),
                    });
                if sound.formats.len() >= 16 {
                    return Err(StorageError::Capacity);
                }
                sound.formats.push(SoundFormat {
                    language: language.to_owned(),
                    format: codec.to_owned(),
                });
            }
        }
        Ok(())
    }
}

pub(super) fn catalogue(
    root: &Path,
    language: Option<&str>,
    format: Option<&str>,
    mut service: impl FnMut() -> Result<(), StorageError>,
) -> Result<Vec<Sound>, StorageError> {
    let directory_path = root.join("sounds");
    match directory(&directory_path) {
        Ok(()) => {}
        Err(error) if missing(&error) => return Ok(Vec::new()),
        Err(error) => return Err(error),
    }
    let mut scan = Scan {
        visited: 0,
        sounds: BTreeMap::new(),
    };
    let mut languages = 0;
    for entry in std::fs::read_dir(directory_path)? {
        service()?;
        let entry = entry?;
        scan.visit()?;
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            return Err(StorageError::InvalidName);
        }
        if !kind.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if name.len() > 64 || ChannelId::try_from(name.to_owned()).is_err() {
            continue;
        }
        languages += 1;
        if languages > 16 {
            return Err(StorageError::Capacity);
        }
        scan.walk(root, name, "", 0, &mut service)?;
    }
    let mut sounds = Vec::with_capacity(scan.sounds.len());
    for mut sound in scan.sounds.into_values() {
        sound.formats.retain(|pair| {
            language.is_none_or(|language| pair.language == language)
                && format.is_none_or(|format| pair.format == format)
        });
        sound.formats.sort_by(|left, right| {
            left.language
                .cmp(&right.language)
                .then(left.format.cmp(&right.format))
        });
        if !sound.formats.is_empty() {
            sounds.push(sound);
        }
    }
    Ok(sounds)
}

#[cfg(test)]
mod tests;
