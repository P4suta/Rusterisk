use std::ffi::OsString;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OptionError {
    Unknown,
    Missing,
    Duplicate,
    Capacity,
    Invalid,
    Profile(&'static str),
}

impl std::fmt::Display for OptionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Unknown => "Unknown option",
            Self::Missing => {
                "Supply --password-stdin and either --profile or both --server and --username"
            }
            Self::Duplicate => "Duplicate option",
            Self::Capacity => "Command arguments exceed capacity",
            Self::Invalid => "Invalid command argument",
            Self::Profile(message) => message,
        })
    }
}

pub struct RemoteOptions {
    pub server: String,
    pub username: String,
}

impl RemoteOptions {
    pub fn parse(arguments: impl Iterator<Item = OsString>) -> Result<Self, OptionError> {
        let (options, json) = Self::parse_with_output(arguments)?;
        if json {
            return Err(OptionError::Unknown);
        }
        Ok(options)
    }

    pub fn parse_with_output(
        mut arguments: impl Iterator<Item = OsString>,
    ) -> Result<(Self, bool), OptionError> {
        let mut json = false;
        let mut server = None;
        let mut username = None;
        let mut profile = None;
        let mut password_stdin = false;
        while let Some(argument) = arguments.next() {
            match argument.to_str() {
                Some(option @ ("--server" | "--username" | "--profile")) => {
                    let (destination, limit) = match option {
                        "--server" => (&mut server, 2048),
                        "--username" => (&mut username, 128),
                        _ => (&mut profile, 64),
                    };
                    if destination.is_some() {
                        return Err(OptionError::Duplicate);
                    }
                    let value = arguments.next().ok_or(OptionError::Missing)?;
                    if value.is_empty() {
                        return Err(OptionError::Missing);
                    }
                    if value.as_encoded_bytes().len() > limit {
                        return Err(OptionError::Capacity);
                    }
                    *destination = Some(value.into_string().map_err(|_| OptionError::Invalid)?);
                }
                Some("--password-stdin") if !password_stdin => password_stdin = true,
                Some("--password-stdin") => return Err(OptionError::Duplicate),
                Some("--json") if !json => json = true,
                Some("--json") => return Err(OptionError::Duplicate),
                Some(_) | None => return Err(OptionError::Unknown),
            }
        }
        if !password_stdin {
            return Err(OptionError::Missing);
        }
        if let Some(name) = profile {
            if server.is_some() || username.is_some() {
                return Err(OptionError::Profile(
                    "Use --profile separately from --server and --username",
                ));
            }
            return Ok((crate::profiles::select(&name)?, json));
        }
        Ok((
            Self {
                server: server.ok_or(OptionError::Missing)?,
                username: username.ok_or(OptionError::Missing)?,
            },
            json,
        ))
    }
}
