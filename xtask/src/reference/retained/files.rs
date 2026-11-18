use super::super::commands::{read, write};
use std::{collections::BTreeSet, ffi::OsString, fs, io, path::Path};

#[derive(Clone, Copy)]
enum Operation {
    Copy,
    Compare,
}

#[derive(Clone, Copy)]
enum Profile {
    Exact,
    State,
    Case,
}

pub(super) fn copy(source: &Path, destination: &Path) -> io::Result<()> {
    apply(source, destination, Operation::Copy, Profile::State)
}

pub(super) fn copy_initial(source: &Path, destination: &Path) -> io::Result<()> {
    apply(source, destination, Operation::Copy, Profile::Exact)
}

pub(super) fn copy_case(source: &Path, destination: &Path) -> io::Result<()> {
    apply(source, destination, Operation::Copy, Profile::Case)
}

pub(super) fn compare(source: &Path, destination: &Path) -> io::Result<()> {
    apply(source, destination, Operation::Compare, Profile::Exact)
}

pub(super) fn compare_case(source: &Path, destination: &Path) -> io::Result<()> {
    apply(source, destination, Operation::Compare, Profile::Case)
}

fn apply(
    source: &Path,
    destination: &Path,
    operation: Operation,
    profile: Profile,
) -> io::Result<()> {
    Tree {
        source,
        destination,
        operation,
        profile,
        entries: 0,
        files: 0,
        bytes: 0,
    }
    .walk(Path::new(""), 0)
}

struct Tree<'a> {
    source: &'a Path,
    destination: &'a Path,
    operation: Operation,
    profile: Profile,
    entries: usize,
    files: usize,
    bytes: usize,
}

fn generated(path: &Path) -> bool {
    path == Path::new("websocket_client.conf")
        || path.starts_with("sounds")
        || path.starts_with("native/data/sounds")
        || path == Path::new("native/data/.rusterisk.lock")
        || path == Path::new("native/data/recordings")
}

impl Tree<'_> {
    fn directory(&self, relative: &Path) -> io::Result<BTreeSet<OsString>> {
        let mut remaining = BTreeSet::new();
        match self.operation {
            Operation::Copy => fs::create_dir(self.destination.join(relative))?,
            Operation::Compare => {
                if !fs::symlink_metadata(self.destination.join(relative))?.is_dir() {
                    return Err(io::Error::other(
                        "reference configuration directory differs",
                    ));
                }
                for entry in fs::read_dir(self.destination.join(relative))? {
                    if remaining.len() >= 4096 {
                        return Err(io::Error::other(
                            "reference configuration inventory exceeds its limit",
                        ));
                    }
                    remaining.insert(entry?.file_name());
                }
            }
        }
        Ok(remaining)
    }

    fn excluded(&self, path: &Path, kind: fs::FileType) -> io::Result<bool> {
        if matches!(self.profile, Profile::State) && path == Path::new("sipp/native/data") {
            if !kind.is_dir() {
                return Err(io::Error::other(
                    "reference native data is not a physical directory",
                ));
            }
            return Ok(true);
        }
        if matches!(
            (self.profile, self.operation),
            (Profile::Case, Operation::Copy)
        ) && matches!(
            path.to_str(),
            Some(
                "native/data/calls.sqlite3"
                    | "native/data/calls.sqlite3-wal"
                    | "native/data/calls.sqlite3-shm"
            )
        ) {
            if !kind.is_file() {
                return Err(io::Error::other(
                    "reference generated database is not a regular file",
                ));
            }
            return Ok(true);
        }
        Ok(false)
    }

    fn file(&mut self, path: &Path) -> io::Result<()> {
        let bytes = read(&self.source.join(path), 8 * 1024 * 1024)?;
        self.files += 1;
        self.bytes = self
            .bytes
            .checked_add(bytes.len())
            .ok_or_else(|| io::Error::other("reference retained size overflowed"))?;
        if self.files > 2048 || self.bytes > 64 * 1024 * 1024 {
            return Err(io::Error::other(
                "reference retained input exceeds its inventory limit",
            ));
        }
        match self.operation {
            Operation::Copy => write(&self.destination.join(path), &bytes),
            Operation::Compare => {
                if read(&self.destination.join(path), 8 * 1024 * 1024)? != bytes {
                    return Err(io::Error::other("reference configuration bytes differ"));
                }
                Ok(())
            }
        }
    }

    fn walk(&mut self, relative: &Path, depth: usize) -> io::Result<()> {
        if depth > 16 || !fs::symlink_metadata(self.source.join(relative))?.is_dir() {
            return Err(io::Error::other(
                "reference retained input is not a bounded directory",
            ));
        }
        let mut remaining = self.directory(relative)?;
        for entry in fs::read_dir(self.source.join(relative))? {
            let entry = entry?;
            self.entries += 1;
            if self.entries > 4096 {
                return Err(io::Error::other(
                    "reference retained input exceeds its entry limit",
                ));
            }
            let path = relative.join(entry.file_name());
            remaining.remove(&entry.file_name());
            let kind = entry.file_type()?;
            if self.excluded(&path, kind)? {
                continue;
            }
            if kind.is_dir() {
                self.walk(&path, depth + 1)?;
            } else if kind.is_file() {
                self.file(&path)?;
            } else {
                return Err(io::Error::other(
                    "reference retained input is not a regular file or directory",
                ));
            }
        }
        if remaining
            .iter()
            .any(|name| !matches!(self.profile, Profile::Case) || !generated(&relative.join(name)))
        {
            return Err(io::Error::other(
                "reference configuration contains additional files",
            ));
        }
        Ok(())
    }
}
