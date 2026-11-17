use std::{io, path::Path};
use xtask::{
    process::CommandSupervisor,
    reference::{compare_sipp_registrations_with, SippAdapter},
};

#[test]
#[ignore = "requires the checked reference image and a freshly built native server"]
fn both_real_servers_complete_the_same_authenticated_registration_and_removal() -> io::Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or_else(|| io::Error::other("workspace missing"))?;
    let executable = root.join("target/debug/rusterisk");
    let mut supervisor = CommandSupervisor::new().map_err(io::Error::other)?;
    let operation = compare_sipp_registrations_with(root, &executable, &mut supervisor);
    let cleanup = supervisor.finish().map_err(io::Error::other);
    match (operation, cleanup) {
        (Ok(comparison), Ok(())) => {
            assert_eq!(comparison.completed_calls(SippAdapter::Asterisk), 1);
            assert_eq!(comparison.completed_calls(SippAdapter::Rusterisk), 1);
            Ok(())
        }
        (Err(operation), Ok(())) | (Ok(_), Err(operation)) => Err(operation),
        (Err(operation), Err(cleanup)) => Err(io::Error::other(format!(
            "{operation}; comparison supervisor cleanup failed: {cleanup}"
        ))),
    }
}
