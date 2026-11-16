#![cfg(unix)]
use std::{io, path::Path, time::Duration};
use xtask::{process::CommandSupervisor, server_distribution::verify_server_native_with};

#[test]
fn native_verification_requires_existing_candidate_bytes_before_running_a_client() -> io::Result<()>
{
    let mut supervisor = CommandSupervisor::new().map_err(io::Error::other)?;
    let mut called = false;
    let result = verify_server_native_with(
        Path::new("missing-native-candidate"),
        Path::new("missing-native-execution"),
        Path::new("missing-native-artifacts"),
        &mut supervisor,
        |service, _address| {
            called = true;
            service
                .run(
                    &mut std::process::Command::new("never-run"),
                    Duration::from_secs(1),
                )
                .map_err(io::Error::other)
        },
    );
    assert!(result.is_err());
    assert!(!called);
    supervisor.finish().map_err(io::Error::other)
}
