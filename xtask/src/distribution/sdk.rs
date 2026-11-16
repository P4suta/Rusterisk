use std::{io::Read, path::Path};

const MAX_INPUT: usize = 65536;
const README: &str = "docs/rust-sdk.md";

#[derive(Debug)]
pub enum SdkInputError {
    Input {
        path: String,
        source: std::io::Error,
    },
    NotRegular {
        path: String,
    },
    Size {
        path: String,
    },
    Encoding {
        path: String,
    },
    Manifest {
        path: String,
        source: toml::de::Error,
    },
    Workspace {
        field: &'static str,
    },
    Metadata {
        package: SdkPackage,
        field: &'static str,
    },
    License {
        package: SdkPackage,
        file: &'static str,
    },
    Documentation,
}

impl std::fmt::Display for SdkInputError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Input { path, source } => write!(formatter, "{path}: {source}"),
            Self::NotRegular { path } => {
                write!(formatter, "{path}: require regular files without symlinks")
            }
            Self::Size { path } => write!(formatter, "{path}: require 1 through {MAX_INPUT} bytes"),
            Self::Encoding { path } => write!(formatter, "{path}: require UTF-8 text"),
            Self::Manifest { path, source } => write!(formatter, "{path}: {source}"),
            Self::Workspace { field } => {
                write!(formatter, "Cargo.toml: invalid workspace.package.{field}")
            }
            Self::Metadata { package, field } => write!(
                formatter,
                "{}/Cargo.toml: invalid package.{field}",
                package.directory()
            ),
            Self::License { package, file } => write!(
                formatter,
                "{}/{file}: must match the root license bytes",
                package.directory()
            ),
            Self::Documentation => write!(formatter, "{README}: require package documentation"),
        }
    }
}

impl std::error::Error for SdkInputError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SdkPackage {
    Core,
    Api,
    Client,
}

impl SdkPackage {
    pub const ALL: [Self; 3] = [Self::Core, Self::Api, Self::Client];

    pub const fn directory(self) -> &'static str {
        match self {
            Self::Core => "crates/core",
            Self::Api => "crates/api",
            Self::Client => "crates/client",
        }
    }
}

fn input(
    read: &mut impl FnMut(&str) -> Result<Vec<u8>, SdkInputError>,
    path: &str,
) -> Result<Vec<u8>, SdkInputError> {
    let bytes = read(path)?;
    if bytes.is_empty() || bytes.len() > MAX_INPUT {
        return Err(SdkInputError::Size {
            path: path.to_owned(),
        });
    }
    Ok(bytes)
}

fn text<'a>(path: &str, bytes: &'a [u8]) -> Result<&'a str, SdkInputError> {
    std::str::from_utf8(bytes).map_err(|_| SdkInputError::Encoding {
        path: path.to_owned(),
    })
}

fn manifest(path: &str, bytes: &[u8]) -> Result<toml::Value, SdkInputError> {
    toml::from_str(text(path, bytes)?).map_err(|source| SdkInputError::Manifest {
        path: path.to_owned(),
        source,
    })
}

fn check_metadata(package: SdkPackage, source: &toml::Value) -> Result<(), SdkInputError> {
    let metadata = source.get("package");
    for field in ["license", "repository"] {
        if metadata
            .and_then(|value| value.get(field))
            .and_then(|value| value.get("workspace"))
            .and_then(toml::Value::as_bool)
            != Some(true)
        {
            return Err(SdkInputError::Metadata { package, field });
        }
    }
    if metadata
        .and_then(|value| value.get("description"))
        .and_then(toml::Value::as_str)
        .is_none_or(|value| value.trim().is_empty())
    {
        return Err(SdkInputError::Metadata {
            package,
            field: "description",
        });
    }
    if metadata
        .and_then(|value| value.get("readme"))
        .and_then(toml::Value::as_str)
        != Some("../../docs/rust-sdk.md")
    {
        return Err(SdkInputError::Metadata {
            package,
            field: "readme",
        });
    }
    Ok(())
}

fn read(root: &Path, path: &str) -> Result<Vec<u8>, SdkInputError> {
    let io_error = |source| SdkInputError::Input {
        path: path.to_owned(),
        source,
    };
    let mut current = root.to_owned();
    let mut components = Path::new(path).components().peekable();
    while let Some(component) = components.next() {
        current.push(component);
        let kind = std::fs::symlink_metadata(&current)
            .map_err(io_error)?
            .file_type();
        if kind.is_symlink()
            || if components.peek().is_some() {
                !kind.is_dir()
            } else {
                !kind.is_file()
            }
        {
            return Err(SdkInputError::NotRegular {
                path: path.to_owned(),
            });
        }
    }
    let mut bytes = Vec::new();
    std::fs::File::open(current)
        .map_err(io_error)?
        .take((MAX_INPUT + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(io_error)?;
    Ok(bytes)
}

pub fn check_sdk_inputs(root: &Path) -> Result<(), SdkInputError> {
    check_sdk_inputs_with(|path| read(root, path))
}

pub fn check_sdk_inputs_with(
    mut read: impl FnMut(&str) -> Result<Vec<u8>, SdkInputError>,
) -> Result<(), SdkInputError> {
    let root = manifest("Cargo.toml", &input(&mut read, "Cargo.toml")?)?;
    let workspace = root.get("workspace").and_then(|value| value.get("package"));
    if workspace
        .and_then(|value| value.get("license"))
        .and_then(toml::Value::as_str)
        != Some("MIT OR Apache-2.0")
    {
        return Err(SdkInputError::Workspace { field: "license" });
    }
    if workspace
        .and_then(|value| value.get("repository"))
        .and_then(toml::Value::as_str)
        .is_none_or(|value| value.trim().is_empty())
    {
        return Err(SdkInputError::Workspace {
            field: "repository",
        });
    }
    let mit = input(&mut read, "LICENSE-MIT")?;
    let apache = input(&mut read, "LICENSE-APACHE")?;
    let documentation = input(&mut read, README)?;
    if text(README, &documentation)?.trim().is_empty() {
        return Err(SdkInputError::Documentation);
    }
    for package in SdkPackage::ALL {
        let path = format!("{}/Cargo.toml", package.directory());
        check_metadata(package, &manifest(&path, &input(&mut read, &path)?)?)?;
        for (file, canonical) in [("LICENSE-MIT", &mit), ("LICENSE-APACHE", &apache)] {
            if input(&mut read, &format!("{}/{file}", package.directory()))? != *canonical {
                return Err(SdkInputError::License { package, file });
            }
        }
    }
    Ok(())
}
