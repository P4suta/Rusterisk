use crate::{Config, ConfigError, Ini, Section};

const FILE: &str = "musiconhold.conf";
const MAX_CLASSES: usize = 32;
const MAX_CLASS_NAME: usize = 79;
const MAX_DIRECTORY: usize = 255;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MusicOnHold {
    classes: Vec<MusicFilesClass>,
}

impl MusicOnHold {
    pub fn classes(&self) -> &[MusicFilesClass] {
        &self.classes
    }

    pub fn class(&self, name: &str) -> Option<&MusicFilesClass> {
        self.classes
            .iter()
            .find(|class| class.name.as_str().eq_ignore_ascii_case(name))
    }

    fn parse(source: &str) -> Result<Self, ConfigError> {
        let mut configuration = Self::default();
        let mut general_seen = false;
        for section in Ini::parse(FILE, source)?.sections {
            if section.name.eq_ignore_ascii_case("general") {
                if general_seen {
                    return Err(ConfigError::at(
                        FILE,
                        section.line,
                        "duplicate general section",
                    ));
                }
                section.check(FILE, &[])?;
                general_seen = true;
                continue;
            }
            if configuration.class(&section.name).is_some() {
                return Err(ConfigError::at(FILE, section.line, "duplicate music class"));
            }
            if configuration.classes.len() >= MAX_CLASSES {
                return Err(ConfigError::at(
                    FILE,
                    section.line,
                    "music classes exceed 32",
                ));
            }
            configuration.classes.push(MusicFilesClass::parse(section)?);
        }
        Ok(configuration)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MusicClassName(String);

impl MusicClassName {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn parse(value: String, line: usize) -> Result<Self, ConfigError> {
        if value.is_empty() || value.len() > MAX_CLASS_NAME {
            return Err(ConfigError::at(
                FILE,
                line,
                "music class name must contain 1 to 79 bytes",
            ));
        }
        Ok(Self(value))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MusicDirectory(String);

impl MusicDirectory {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn parse(value: &str, line: usize) -> Result<Self, ConfigError> {
        if value.is_empty()
            || value.len() > MAX_DIRECTORY
            || value.chars().any(char::is_control)
            || !value.split('/').all(portable_component)
        {
            return Err(ConfigError::at(
                FILE,
                line,
                "directory requires 1 to 255 bytes of portable relative components",
            ));
        }
        Ok(Self(value.into()))
    }
}

fn portable_component(component: &str) -> bool {
    !component.is_empty()
        && !component.ends_with([' ', '.'])
        && !component.contains(['<', '>', ':', '"', '\\', '|', '?', '*'])
        && !reserved_device(component)
}

fn reserved_device(component: &str) -> bool {
    let basename = component
        .split_once('.')
        .map_or(component, |(basename, _)| basename);
    if ["CON", "PRN", "AUX", "NUL"]
        .iter()
        .any(|device| basename.eq_ignore_ascii_case(device))
    {
        return true;
    }
    let Some(prefix) = basename.get(..3) else {
        return false;
    };
    ["COM", "LPT"]
        .iter()
        .any(|device| prefix.eq_ignore_ascii_case(device))
        && matches!(
            basename.get(3..),
            Some("1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³")
        )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MusicSort {
    Alpha,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MusicFilesClass {
    name: MusicClassName,
    directory: MusicDirectory,
    sort: MusicSort,
    directory_line: usize,
}

impl MusicFilesClass {
    pub fn name(&self) -> &MusicClassName {
        &self.name
    }

    pub fn directory(&self) -> &MusicDirectory {
        &self.directory
    }

    pub fn sort(&self) -> MusicSort {
        self.sort
    }

    pub fn directory_line(&self) -> usize {
        self.directory_line
    }

    fn parse(section: Section) -> Result<Self, ConfigError> {
        for (name, (line, _)) in &section.options {
            if !["mode", "directory", "sort"]
                .iter()
                .any(|supported| name.eq_ignore_ascii_case(supported))
            {
                return Err(ConfigError::at(
                    FILE,
                    *line,
                    format!("unsupported option {name}"),
                ));
            }
        }
        let (mode_line, mode) = required_option(&section, "mode")?;
        if !mode.eq_ignore_ascii_case("files") {
            return Err(ConfigError::at(FILE, mode_line, "mode must be files"));
        }
        let (directory_line, directory) = required_option(&section, "directory")?;
        let directory = MusicDirectory::parse(directory, directory_line)?;
        let sort = match option(&section, "sort")? {
            None => MusicSort::Alpha,
            Some((_, value)) if value.eq_ignore_ascii_case("alpha") => MusicSort::Alpha,
            Some((line, _)) => return Err(ConfigError::at(FILE, line, "sort must be alpha")),
        };
        Ok(Self {
            name: MusicClassName::parse(section.name, section.line)?,
            directory,
            sort,
            directory_line,
        })
    }
}

fn option<'a>(section: &'a Section, key: &str) -> Result<Option<(usize, &'a str)>, ConfigError> {
    let mut found = None;
    for (name, (line, value)) in &section.options {
        if name.eq_ignore_ascii_case(key) {
            if let Some((previous, _)) = found {
                return Err(ConfigError::at(
                    FILE,
                    (*line).max(previous),
                    format!("duplicate option {key}"),
                ));
            }
            found = Some((*line, value.as_str()));
        }
    }
    Ok(found)
}

fn required_option<'a>(section: &'a Section, key: &str) -> Result<(usize, &'a str), ConfigError> {
    let (line, value) = option(section, key)?
        .ok_or_else(|| ConfigError::at(FILE, section.line, format!("missing {key}")))?;
    if value.is_empty() {
        return Err(ConfigError::at(FILE, line, format!("missing {key}")));
    }
    Ok((line, value))
}

impl Config {
    pub fn with_music_on_hold_configuration(mut self, source: &str) -> Result<Self, ConfigError> {
        self.music_on_hold = MusicOnHold::parse(source)?;
        Ok(self)
    }
}
