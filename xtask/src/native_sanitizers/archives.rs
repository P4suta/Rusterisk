use serde_json::Value;
use std::{collections::BTreeMap, io};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum NativeLibrary {
    Opus,
    Sqlite,
    AwsLc,
}

impl NativeLibrary {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Opus => "opus",
            Self::Sqlite => "sqlite",
            Self::AwsLc => "aws-lc",
        }
    }

    fn package(self) -> &'static str {
        match self {
            Self::Opus => "opusic-sys",
            Self::Sqlite => "libsqlite3-sys",
            Self::AwsLc => "aws-lc-sys",
        }
    }
}

pub struct NativeArchive {
    library: NativeLibrary,
    paths: Vec<String>,
}

impl NativeArchive {
    pub const fn library(&self) -> NativeLibrary {
        self.library
    }

    pub fn paths(&self) -> &[String] {
        &self.paths
    }
}

fn invalid() -> io::Error {
    io::Error::other("native build requires linked instrumented Opus, SQLite, and AWS-LC archives")
}

fn text(bytes: &[u8]) -> io::Result<&str> {
    if bytes.len() > 4 * 1024 * 1024 {
        return Err(invalid());
    }
    std::str::from_utf8(bytes).map_err(|_| invalid())
}

fn safe_path(path: &str) -> bool {
    path.starts_with("/state/target/")
        && path.len() <= 4096
        && path
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_./-".contains(&byte))
        && path[1..]
            .split('/')
            .all(|part| !matches!(part, "" | "." | ".."))
}

fn archive(library: NativeLibrary, record: &Value) -> io::Result<NativeArchive> {
    let directory = record["out_dir"].as_str().ok_or_else(invalid)?;
    let owner = directory
        .strip_prefix("/state/target/debug/build/")
        .and_then(|path| path.strip_suffix("/out"))
        .ok_or_else(invalid)?;
    if !safe_path(directory)
        || owner.contains('/')
        || !owner
            .strip_prefix(library.package())
            .is_some_and(|hash| hash.starts_with('-') && hash.len() > 1)
    {
        return Err(invalid());
    }
    let libs = record["linked_libs"].as_array().ok_or_else(invalid)?;
    if libs.len() > 32 {
        return Err(invalid());
    }
    let mut name = None;
    for value in libs {
        let linked = value.as_str().ok_or_else(invalid)?;
        let Some(linked) = linked.strip_prefix("static=") else {
            continue;
        };
        let selected = match library {
            NativeLibrary::Opus => linked == "opus",
            NativeLibrary::Sqlite => linked == "sqlite3",
            NativeLibrary::AwsLc => linked.starts_with("aws_lc_") && linked.ends_with("_crypto"),
        };
        if selected
            && (!linked
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
                || name.replace(linked).is_some())
        {
            return Err(invalid());
        }
    }
    let name = name.ok_or_else(invalid)?;
    let directories = record["linked_paths"].as_array().ok_or_else(invalid)?;
    if directories.len() > 32 {
        return Err(invalid());
    }
    let mut paths = Vec::new();
    for value in directories {
        let value = value.as_str().ok_or_else(invalid)?;
        let path = value.strip_prefix("native=").unwrap_or(value);
        if !safe_path(path)
            || !(path == directory
                || path
                    .strip_prefix(directory)
                    .is_some_and(|tail| tail.starts_with('/')))
        {
            return Err(invalid());
        }
        let file = format!("{path}/lib{name}.a");
        if paths.contains(&file) {
            return Err(invalid());
        }
        paths.push(file);
    }
    if paths.is_empty() {
        return Err(invalid());
    }
    Ok(NativeArchive { library, paths })
}

pub fn native_archives(bytes: &[u8]) -> io::Result<Vec<NativeArchive>> {
    let mut archives = BTreeMap::new();
    let mut finished = false;
    for line in text(bytes)?.lines().filter(|line| line.starts_with('{')) {
        let record: Value = serde_json::from_str(line).map_err(|_| invalid())?;
        if finished {
            return Err(invalid());
        }
        match record["reason"].as_str() {
            Some("build-finished") if record["success"].as_bool() == Some(true) => finished = true,
            Some("build-finished") => return Err(invalid()),
            Some("build-script-executed") => {
                let package = record["package_id"].as_str().ok_or_else(invalid)?;
                let name = package
                    .rsplit('#')
                    .next()
                    .and_then(|package| package.split_once('@'))
                    .map(|(name, _)| name);
                let library = match name {
                    Some("opusic-sys") => NativeLibrary::Opus,
                    Some("libsqlite3-sys") => NativeLibrary::Sqlite,
                    Some("aws-lc-sys") => NativeLibrary::AwsLc,
                    _ => continue,
                };
                if archives
                    .insert(library, archive(library, &record)?)
                    .is_some()
                {
                    return Err(invalid());
                }
            }
            Some("compiler-artifact" | "compiler-message") => {}
            _ => return Err(invalid()),
        }
    }
    if !finished || archives.len() != 3 {
        return Err(invalid());
    }
    Ok(archives.into_values().collect())
}

pub fn validate_instrumented_archive(bytes: &[u8]) -> io::Result<()> {
    let mut address = false;
    let mut undefined = false;
    for line in text(bytes)?.lines() {
        let mut fields = line.split_whitespace();
        if fields.next() != Some("U") {
            continue;
        }
        if let Some(symbol) = fields.next() {
            if fields.next().is_some() {
                return Err(invalid());
            }
            address |= symbol.starts_with("__asan_report_");
            undefined |= symbol.starts_with("__ubsan_handle_");
        }
    }
    if address && undefined {
        Ok(())
    } else {
        Err(invalid())
    }
}
