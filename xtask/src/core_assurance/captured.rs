use super::{snapshot::Snapshot, AssuranceError};
use std::{collections::BTreeSet, path::Path};

pub(crate) struct CapturedAssurance {
    snapshot: Snapshot,
}
impl CapturedAssurance {
    pub(crate) fn read(core: &Path) -> Result<Self, AssuranceError> {
        Ok(Self {
            snapshot: Snapshot::read(core)?,
        })
    }
    pub(crate) fn inputs(&self) -> impl ExactSizeIterator<Item = (&Path, &[u8])> {
        self.snapshot
            .files
            .iter()
            .map(|file| (file.path.as_path(), file.bytes.as_slice()))
    }
    pub(crate) fn normalized(&self) -> &str {
        &self.snapshot.normalized
    }
    pub(crate) fn targets(&self) -> &BTreeSet<String> {
        &self.snapshot.targets
    }
    pub(crate) fn tests(&self) -> &BTreeSet<String> {
        &self.snapshot.tests
    }
}
