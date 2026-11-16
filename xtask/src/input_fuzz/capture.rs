use super::{snapshot::Snapshot, FuzzError};
use std::path::{Path, PathBuf};

pub struct CapturedInputFuzz {
    snapshot: Snapshot,
    root: PathBuf,
}
impl CapturedInputFuzz {
    pub fn read(root: &Path) -> Result<Self, FuzzError> {
        let snapshot = Snapshot::read(root)?;
        let target_spec = snapshot
            .files
            .iter()
            .find(|file| file.path == Path::new("xtask/fuzz/src/targets.rs"))
            .ok_or(FuzzError::Manifest)?;
        if target_spec.bytes != include_bytes!("../../fuzz/src/targets.rs") {
            return Err(FuzzError::SourceChanged);
        }
        Ok(Self {
            snapshot,
            root: root.canonicalize()?,
        })
    }
    pub fn inputs(&self) -> impl ExactSizeIterator<Item = (&Path, &[u8])> {
        self.snapshot
            .files
            .iter()
            .map(|file| (file.path.as_path(), file.bytes.as_slice()))
    }
    pub(super) fn root(&self) -> &Path {
        &self.root
    }
    pub(super) fn normalized(&self) -> &str {
        &self.snapshot.normalized
    }
    pub(super) fn write(&self, source: &Path, state: &Path) -> Result<(), FuzzError> {
        self.snapshot.write(source, state)
    }
    pub(super) fn check_source(&self, source: &Path) -> Result<(), FuzzError> {
        self.snapshot.verify_source(source)
    }
    pub(super) fn check_harness(&self, state: &Path) -> Result<(), FuzzError> {
        self.snapshot.check_harness(state)
    }
}
