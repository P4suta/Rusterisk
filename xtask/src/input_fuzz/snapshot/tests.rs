use super::{FuzzError, Snapshot};
use crate::process::ProcessError;
use std::{
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

#[test]
fn captured_sources_remain_bound_while_independent_checkout_edits_continue() -> Result<(), FuzzError>
{
    let project = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or(FuzzError::Manifest)?;
    let snapshot = Snapshot::read(project)?;
    let identity = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| FuzzError::Plan)?
        .as_nanos();
    let directory = std::env::temp_dir().join(format!(
        "rusterisk-fuzz-snapshot-{}-{identity}",
        std::process::id()
    ));
    std::fs::create_dir(&directory)?;
    let result = (|| {
        let checkout = directory.join("checkout");
        let source = directory.join("source");
        snapshot.write(&checkout, &directory.join("checkout-state"))?;
        snapshot.write(&source, &directory.join("state"))?;
        snapshot.verify_source(&source)?;
        std::fs::write(
            checkout.join("crates/config/src/lib.rs"),
            b"changed checkout",
        )?;
        snapshot.verify_source(&source)?;
        std::fs::write(
            source.join("crates/config/src/lib.rs"),
            b"changed captured source",
        )?;
        assert!(matches!(
            snapshot.verify_source(&source),
            Err(FuzzError::SourceChanged)
        ));
        Ok(())
    })();
    match (result, std::fs::remove_dir_all(directory)) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(error)) => Err(FuzzError::Io(error)),
        (Err(error), Err(cleanup)) => Err(FuzzError::Cleanup {
            operation: error.to_string(),
            cleanup: ProcessError::Io(cleanup),
        }),
    }
}
