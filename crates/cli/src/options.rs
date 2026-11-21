use std::ffi::OsString;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OptionError {
    Unknown,
    Missing,
    Duplicate,
    Capacity,
    Invalid,
}

impl std::fmt::Display for OptionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Unknown => "Unknown option",
            Self::Missing => "Supply --server, --username, and --password-stdin",
            Self::Duplicate => "Duplicate option",
            Self::Capacity => "Command arguments exceed capacity",
            Self::Invalid => "Invalid command argument",
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
        let mut password_stdin = false;
        while let Some(argument) = arguments.next() {
            match argument.to_str() {
                Some(option @ ("--server" | "--username")) => {
                    let (destination, limit) = if option == "--server" {
                        (&mut server, 2048)
                    } else {
                        (&mut username, 128)
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
        Ok((
            Self {
                server: server.ok_or(OptionError::Missing)?,
                username: username.ok_or(OptionError::Missing)?,
            },
            json,
        ))
    }
}
