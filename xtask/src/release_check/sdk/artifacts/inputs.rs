use super::super::{load_paired, CapturedReleaseSource, InputKey, SdkRunError};
use crate::{
    distribution::{CapturedSdk, SdkContents, SdkPackage},
    release_check::candidate_run::copy_checked,
};
use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

pub(super) struct Loaded {
    pub(super) contents: SdkContents,
    pub(super) projection: InputKey,
    source: CapturedSdk,
    root: PathBuf,
}

fn write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(bytes)?;
    file.flush()
}

impl Loaded {
    pub(super) fn read(
        captured: &CapturedReleaseSource,
        candidate: &Path,
    ) -> Result<Self, SdkRunError> {
        let (source, contents, projection) = load_paired(captured, candidate)?;
        Ok(Self {
            source,
            contents,
            projection,
            root: candidate.to_owned(),
        })
    }

    pub(super) fn same(&self, other: &Self) -> io::Result<()> {
        if self.projection != other.projection
            || !self.source.files().eq(other.source.files())
            || self.contents.version() != other.contents.version()
            || self.contents.source_digest() != other.contents.source_digest()
            || SdkPackage::ALL.iter().any(|package| {
                self.contents.checksum(*package) != other.contents.checksum(*package)
            })
        {
            return Err(io::Error::other(
                "SDK artifact candidate bytes or source projections differ",
            ));
        }
        Ok(())
    }

    pub(super) fn check(&self, captured: &CapturedReleaseSource) -> io::Result<()> {
        let current = Self::read(captured, &self.root).map_err(io::Error::other)?;
        self.same(&current)
    }

    pub(super) fn preserve(
        &self,
        captured: &CapturedReleaseSource,
        native: &Path,
    ) -> io::Result<()> {
        let sdk = native.join("sdk-candidate");
        fs::create_dir(&sdk)?;
        self.source
            .write(&sdk.join("source"), &sdk.join("captured-browser"))
            .map_err(io::Error::other)?;
        let mut sums = String::new();
        for package in SdkPackage::ALL {
            let name = format!("{}-{}.crate", package.name(), self.contents.version());
            copy_checked(
                &self.root.join(&name),
                &sdk.join(&name),
                self.contents.checksum(package),
                16 * 1024 * 1024,
            )?;
            sums.push_str(&format!("{}  {name}\n", self.contents.checksum(package)));
        }
        write(&sdk.join("SHA256SUMS"), sums.as_bytes())?;
        write(
            &sdk.join("SOURCE.sha256"),
            format!("{}\n", self.contents.source_digest()).as_bytes(),
        )?;
        captured
            .write(
                &native.join("release-source"),
                &native.join("captured-browser"),
            )
            .map_err(io::Error::other)
    }
}
