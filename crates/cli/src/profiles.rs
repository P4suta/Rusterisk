use crate::{http, OptionError, RemoteOptions};
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    env,
    fs::File,
    io::{ErrorKind, Read},
    path::PathBuf,
};

const MAX_FILE: u64 = 65_536;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileContents {
    profiles: Vec<Profile>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Profile {
    name: String,
    server: String,
    username: String,
}

fn directory() -> Result<PathBuf, OptionError> {
    let absolute = |name| {
        env::var_os(name)
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
    };
    if let Some(path) = absolute("XDG_CONFIG_HOME") {
        return Ok(path);
    }
    if cfg!(windows) {
        return absolute("APPDATA").ok_or(OptionError::Profile(
            "Set APPDATA or XDG_CONFIG_HOME for server profiles",
        ));
    }
    absolute("HOME")
        .map(|path| path.join(".config"))
        .ok_or(OptionError::Profile(
            "Set HOME or XDG_CONFIG_HOME for server profiles",
        ))
}

fn load() -> Result<BTreeMap<String, RemoteOptions>, OptionError> {
    let path = directory()?.join("rusterisk").join("profiles.json");
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(_) => {
            return Err(OptionError::Profile(
                "Cannot open the profile configuration",
            ))
        }
    };
    let mut bytes = Vec::new();
    file.take(MAX_FILE + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| OptionError::Profile("Cannot read the profile configuration"))?;
    if bytes.len() as u64 > MAX_FILE {
        return Err(OptionError::Capacity);
    }
    let document: FileContents = serde_json::from_slice(&bytes)
        .map_err(|_| OptionError::Profile("Invalid profile configuration"))?;
    if document.profiles.len() > 64 {
        return Err(OptionError::Capacity);
    }
    let mut profiles = BTreeMap::new();
    for profile in document.profiles {
        if profile.name.is_empty()
            || profile.name.len() > 64
            || !profile
                .name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
        {
            return Err(OptionError::Profile("Invalid profile name"));
        }
        http::origin(&profile.server)
            .map_err(|_| OptionError::Profile("Invalid profile server origin"))?;
        if !http::valid_username(&profile.username) {
            return Err(OptionError::Profile("Invalid profile username"));
        }
        if profiles
            .insert(
                profile.name,
                RemoteOptions {
                    server: profile.server,
                    username: profile.username,
                },
            )
            .is_some()
        {
            return Err(OptionError::Profile("Duplicate profile name"));
        }
    }
    Ok(profiles)
}

pub(super) fn select(name: &str) -> Result<RemoteOptions, OptionError> {
    load()?
        .remove(name)
        .ok_or(OptionError::Profile("Unknown server profile"))
}

pub fn profile_names() -> Result<Vec<String>, OptionError> {
    Ok(load()?.into_keys().collect())
}
