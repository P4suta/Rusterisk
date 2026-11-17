use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    fmt,
    fs::File,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandField {
    Program,
    Argument,
    EnvironmentName,
    EnvironmentValue,
    Directory,
}

impl fmt::Display for CommandField {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Program => "program",
            Self::Argument => "argument",
            Self::EnvironmentName => "environment name",
            Self::EnvironmentValue => "environment value",
            Self::Directory => "working directory",
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandRequestError {
    Empty(CommandField),
    Nul(CommandField),
    FieldTooLong(CommandField),
    InvalidEnvironmentName,
    TooManyArguments,
    TooManyEnvironmentVariables,
    TotalTooLong,
}

impl fmt::Display for CommandRequestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty(field) => write!(formatter, "command {field} is empty"),
            Self::Nul(field) => write!(formatter, "command {field} contains NUL"),
            Self::FieldTooLong(field) => {
                write!(formatter, "command {field} exceeds its byte limit")
            }
            Self::InvalidEnvironmentName => {
                formatter.write_str("command environment name contains =")
            }
            Self::TooManyArguments => {
                formatter.write_str("command argument count exceeds its limit")
            }
            Self::TooManyEnvironmentVariables => {
                formatter.write_str("command environment count exceeds its limit")
            }
            Self::TotalTooLong => formatter.write_str("command input exceeds its total byte limit"),
        }
    }
}

impl std::error::Error for CommandRequestError {}

pub enum CommandStream {
    Null,
    Inherit,
    File(File),
}

impl fmt::Debug for CommandStream {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Null => "Null",
            Self::Inherit => "Inherit",
            Self::File(_) => "File",
        })
    }
}

impl CommandStream {
    fn into_stdio(self) -> Stdio {
        match self {
            Self::Null => Stdio::null(),
            Self::Inherit => Stdio::inherit(),
            Self::File(file) => Stdio::from(file),
        }
    }
}

enum EnvironmentBase {
    Inherit,
    Clear,
}

/// A bounded command request with private values omitted from diagnostics.
pub struct SupervisedCommand {
    program: OsString,
    arguments: Vec<OsString>,
    environment_base: EnvironmentBase,
    environment: BTreeMap<OsString, Option<OsString>>,
    directory: Option<PathBuf>,
    stdout: CommandStream,
    stderr: CommandStream,
    bytes: usize,
}

impl fmt::Debug for SupervisedCommand {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SupervisedCommand")
            .field("arguments", &self.arguments.len())
            .field("environment", &self.environment.len())
            .field("directory", &self.directory.is_some())
            .field("stdout", &self.stdout)
            .field("stderr", &self.stderr)
            .finish()
    }
}

fn encoded_len(value: &OsStr, field: CommandField) -> Result<usize, CommandRequestError> {
    if value.is_empty()
        && matches!(
            field,
            CommandField::Program | CommandField::EnvironmentName | CommandField::Directory
        )
    {
        return Err(CommandRequestError::Empty(field));
    }
    let bytes = value.as_encoded_bytes();
    if bytes.contains(&0) {
        return Err(CommandRequestError::Nul(field));
    }
    if bytes.len() > SupervisedCommand::MAX_FIELD_BYTES {
        return Err(CommandRequestError::FieldTooLong(field));
    }
    Ok(bytes.len())
}

impl SupervisedCommand {
    pub const MAX_ARGUMENTS: usize = 128;
    pub const MAX_ENVIRONMENT: usize = 64;
    pub const MAX_FIELD_BYTES: usize = 4096;
    /// Counts explicit program, argument, environment, and directory encoded bytes.
    pub const MAX_INPUT_BYTES: usize = 16 * 1024;

    /// Creates a request with null stdin, stdout, and stderr.
    pub fn new(program: impl AsRef<OsStr>) -> Result<Self, CommandRequestError> {
        let program = program.as_ref();
        let bytes = encoded_len(program, CommandField::Program)?;
        Ok(Self {
            program: program.to_os_string(),
            arguments: Vec::new(),
            environment_base: EnvironmentBase::Inherit,
            environment: BTreeMap::new(),
            directory: None,
            stdout: CommandStream::Null,
            stderr: CommandStream::Null,
            bytes,
        })
    }

    fn changed_bytes(&self, removed: usize, added: usize) -> Result<usize, CommandRequestError> {
        self.bytes
            .checked_sub(removed)
            .and_then(|bytes| bytes.checked_add(added))
            .filter(|bytes| *bytes <= Self::MAX_INPUT_BYTES)
            .ok_or(CommandRequestError::TotalTooLong)
    }

    pub fn arg(&mut self, argument: impl AsRef<OsStr>) -> Result<&mut Self, CommandRequestError> {
        let argument = argument.as_ref();
        let length = encoded_len(argument, CommandField::Argument)?;
        if self.arguments.len() >= Self::MAX_ARGUMENTS {
            return Err(CommandRequestError::TooManyArguments);
        }
        let bytes = self.changed_bytes(0, length)?;
        self.arguments.push(argument.to_os_string());
        self.bytes = bytes;
        Ok(self)
    }

    pub fn env(
        &mut self,
        name: impl AsRef<OsStr>,
        value: impl AsRef<OsStr>,
    ) -> Result<&mut Self, CommandRequestError> {
        self.change_environment(name.as_ref(), Some(value.as_ref()))
    }

    /// Records a removal without inheriting the named value.
    pub fn env_remove(
        &mut self,
        name: impl AsRef<OsStr>,
    ) -> Result<&mut Self, CommandRequestError> {
        self.change_environment(name.as_ref(), None)
    }

    fn change_environment(
        &mut self,
        name: &OsStr,
        value: Option<&OsStr>,
    ) -> Result<&mut Self, CommandRequestError> {
        encoded_len(name, CommandField::EnvironmentName)?;
        if let Some(value) = value {
            encoded_len(value, CommandField::EnvironmentValue)?;
        }
        if name.as_encoded_bytes().contains(&b'=') {
            return Err(CommandRequestError::InvalidEnvironmentName);
        }
        let mut command = self.environment_command();
        match value {
            Some(value) => command.env(name, value),
            None => command.env_remove(name),
        };
        self.commit_environment(&command)?;
        Ok(self)
    }

    fn environment_command(&self) -> Command {
        let mut command = Command::new(&self.program);
        match &self.environment_base {
            EnvironmentBase::Inherit => {}
            EnvironmentBase::Clear => {
                command.env_clear();
            }
        }
        for (name, value) in &self.environment {
            match value {
                Some(value) => command.env(name, value),
                None => command.env_remove(name),
            };
        }
        command
    }

    fn commit_environment(&mut self, command: &Command) -> Result<(), CommandRequestError> {
        if command.get_envs().count() > Self::MAX_ENVIRONMENT {
            return Err(CommandRequestError::TooManyEnvironmentVariables);
        }
        let removed = self
            .environment
            .iter()
            .map(|(name, value)| name.len() + value.as_ref().map_or(0, |value| value.len()))
            .sum();
        let added = command
            .get_envs()
            .try_fold(0usize, |total, (name, value)| {
                let name_bytes = encoded_len(name, CommandField::EnvironmentName)?;
                let value_bytes = value
                    .map(|value| encoded_len(value, CommandField::EnvironmentValue))
                    .transpose()?
                    .unwrap_or(0);
                total
                    .checked_add(name_bytes)
                    .and_then(|bytes| bytes.checked_add(value_bytes))
                    .ok_or(CommandRequestError::TotalTooLong)
            })?;
        let bytes = self.changed_bytes(removed, added)?;
        let environment = command
            .get_envs()
            .map(|(name, value)| (name.to_os_string(), value.map(OsStr::to_os_string)))
            .collect();
        self.environment = environment;
        self.bytes = bytes;
        Ok(())
    }

    /// Discards explicit changes and disables inherited environment variables.
    pub fn env_clear(&mut self) -> Result<&mut Self, CommandRequestError> {
        let mut command = self.environment_command();
        command.env_clear();
        self.commit_environment(&command)?;
        self.environment_base = EnvironmentBase::Clear;
        Ok(self)
    }

    pub fn current_dir(
        &mut self,
        directory: impl AsRef<Path>,
    ) -> Result<&mut Self, CommandRequestError> {
        let directory = directory.as_ref();
        let length = encoded_len(directory.as_os_str(), CommandField::Directory)?;
        let removed = self
            .directory
            .as_ref()
            .map_or(0, |directory| directory.as_os_str().len());
        let bytes = self.changed_bytes(removed, length)?;
        self.directory = Some(directory.to_path_buf());
        self.bytes = bytes;
        Ok(self)
    }

    pub fn stdout(&mut self, stream: CommandStream) -> &mut Self {
        self.stdout = stream;
        self
    }

    pub fn stderr(&mut self, stream: CommandStream) -> &mut Self {
        self.stderr = stream;
        self
    }

    /// Transfers owned output files into the standard process adapter.
    pub fn into_std(self) -> Command {
        let mut command = Command::new(self.program);
        command.args(self.arguments);
        match self.environment_base {
            EnvironmentBase::Inherit => {}
            EnvironmentBase::Clear => {
                command.env_clear();
            }
        }
        for (name, value) in self.environment {
            match value {
                Some(value) => command.env(name, value),
                None => command.env_remove(name),
            };
        }
        if let Some(directory) = self.directory {
            command.current_dir(directory);
        }
        command
            .stdin(Stdio::null())
            .stdout(self.stdout.into_stdio())
            .stderr(self.stderr.into_stdio());
        command
    }
}
