use super::CoreMutationError;
use crate::core_assurance::{AssuranceTreeError, CapturedAssuranceTree, FieldMutation};
use std::{collections::BTreeSet, path::Path};

impl From<AssuranceTreeError> for CoreMutationError {
    fn from(error: AssuranceTreeError) -> Self {
        match error {
            AssuranceTreeError::Io(error) => Self::Io(error),
            AssuranceTreeError::Assurance(error) => Self::Assurance(error),
            AssuranceTreeError::Changed => Self::Changed,
            AssuranceTreeError::Plan => Self::Plan,
        }
    }
}

pub struct CapturedCoreMutation {
    tree: CapturedAssuranceTree,
}
impl CapturedCoreMutation {
    pub fn read(root: &Path) -> Result<Self, CoreMutationError> {
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
    pub(super) fn write(&self, original: &Path, working: &Path) -> Result<(), CoreMutationError> {
        self.tree
            .write(original, working)
            .map_err(CoreMutationError::from)
    }
    pub(super) fn write_field(
        &self,
        working: &Path,
        mutation: FieldMutation,
    ) -> Result<(), CoreMutationError> {
        self.tree
            .write_field(working, mutation)
            .map_err(CoreMutationError::from)
    }
    pub(super) fn check(&self, original: &Path, working: &Path) -> Result<(), CoreMutationError> {
        self.tree
            .check(original, working)
            .map_err(CoreMutationError::from)
    }
    pub(super) fn check_field(
        &self,
        original: &Path,
        working: &Path,
        mutation: FieldMutation,
    ) -> Result<(), CoreMutationError> {
        self.tree
            .check_field(original, working, mutation)
            .map_err(CoreMutationError::from)
    }
}
