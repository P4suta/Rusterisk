use super::CoreMiriError;
use crate::core_assurance::{AssuranceTreeError, CapturedAssuranceTree};
use std::{collections::BTreeSet, path::Path};

impl From<AssuranceTreeError> for CoreMiriError {
    fn from(error: AssuranceTreeError) -> Self {
        match error {
            AssuranceTreeError::Io(error) => Self::Io(error),
            AssuranceTreeError::Assurance(error) => Self::Assurance(error),
            AssuranceTreeError::Changed => Self::Changed,
            AssuranceTreeError::Plan => Self::Plan,
        }
    }
}

pub struct CapturedCoreMiri {
    tree: CapturedAssuranceTree,
}
impl CapturedCoreMiri {
    pub fn read(root: &Path) -> Result<Self, CoreMiriError> {
        Ok(Self {
            tree: CapturedAssuranceTree::read(root)?,
        })
    }
    pub fn inputs(&self) -> impl ExactSizeIterator<Item = (&Path, &[u8])> {
        self.tree.inputs()
    }
    pub fn targets(&self) -> &BTreeSet<String> {
        self.tree.targets()
    }
    pub fn tests(&self) -> &BTreeSet<String> {
        self.tree.tests()
    }
    pub(super) fn write(&self, original: &Path, working: &Path) -> Result<(), CoreMiriError> {
        self.tree
            .write(original, working)
            .map_err(CoreMiriError::from)
    }
    pub(super) fn check(&self, original: &Path, working: &Path) -> Result<(), CoreMiriError> {
        self.tree
            .check(original, working)
            .map_err(CoreMiriError::from)
    }
}
