use super::{reusable_candidate, ConnectionKey};
use crate::ControlError;
use std::net::SocketAddr;

#[test]
fn existing_backup_connections_cannot_bypass_the_preferred_resolved_candidate(
) -> Result<(), ControlError> {
    let preferred: SocketAddr = ([127, 0, 0, 1], 5061).into();
    let backup: SocketAddr = ([127, 0, 0, 2], 5061).into();
    let local: SocketAddr = ([127, 0, 0, 1], 5060).into();
    let key = ConnectionKey {
        listener: 0,
        peer: backup,
        generation: 1,
    };
    assert!(
        reusable_candidate([preferred, backup].into_iter(), local, |peer| {
            (peer == backup).then_some(key)
        })?
        .is_none()
    );
    Ok(())
}

#[test]
fn candidate_reuse_keeps_address_family_and_connection_generation() -> Result<(), ControlError> {
    let preferred: SocketAddr = ([127, 0, 0, 1], 5061).into();
    let local: SocketAddr = ([127, 0, 0, 1], 5060).into();
    let key = ConnectionKey {
        listener: 0,
        peer: preferred,
        generation: 9,
    };
    let reused = reusable_candidate(std::iter::once(preferred), local, |peer| {
        (peer == preferred).then_some(key)
    })?;
    assert_eq!(reused, Some(key));
    let unsupported: SocketAddr = ([0_u16, 0, 0, 0, 0, 0, 0, 1], 5061).into();
    assert_eq!(
        reusable_candidate([unsupported, preferred].into_iter(), local, |peer| {
            (peer == preferred).then_some(key)
        })?,
        Some(key)
    );
    assert!(matches!(
        reusable_candidate(std::iter::once(unsupported), local, |_| Some(key)),
        Err(ControlError::NotFound(_))
    ));
    Ok(())
}
