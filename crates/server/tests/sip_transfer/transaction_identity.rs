use crate::failure;
use rusterisk_server::ControlError;
use std::fmt;

#[derive(Clone, Copy)]
pub(super) struct BranchNamespace([u8; 16]);

#[derive(Clone, Copy)]
pub(super) struct Branch {
    namespace: BranchNamespace,
    transaction: u32,
}

impl BranchNamespace {
    pub(super) fn random() -> Result<Self, ControlError> {
        let mut entropy = [0; 16];
        getrandom::fill(&mut entropy).map_err(failure)?;
        Ok(Self(entropy))
    }

    pub(super) const fn branch(self, transaction: u32) -> Branch {
        Branch {
            namespace: self,
            transaction,
        }
    }
}

impl fmt::Display for Branch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "z9hG4bK-{:032x}-{:08x}",
            u128::from_be_bytes(self.namespace.0),
            self.transaction
        )
    }
}

#[test]
fn namespaces_and_transaction_numbers_preserve_fixed_ascii_cookie_identity(
) -> Result<(), ControlError> {
    let mut identities = std::collections::BTreeSet::new();
    for entropy in [[0; 16], [1; 16], [0xff; 16]] {
        let namespace = BranchNamespace(entropy);
        for transaction in [0, 1, u32::MAX] {
            let branch = namespace.branch(transaction).to_string();
            assert_eq!(branch.len(), 49);
            let suffix = branch
                .strip_prefix("z9hG4bK-")
                .ok_or(ControlError::Invalid("Missing branch magic cookie"))?;
            assert!(suffix
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() || byte == b'-'));
            assert!(identities.insert(branch.to_ascii_lowercase()));
        }
        assert_eq!(
            namespace.branch(1).to_string(),
            namespace.branch(1).to_string()
        );
    }
    assert_eq!(identities.len(), 9);
    Ok(())
}
