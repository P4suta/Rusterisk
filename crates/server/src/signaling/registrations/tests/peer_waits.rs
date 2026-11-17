use crate::{
    signaling::registrations::peer_waits::{Admission, PeerWaits},
    ControlError,
};
use std::{net::SocketAddr, num::NonZeroU32};

fn peer(index: u16) -> Result<SocketAddr, ControlError> {
    let port = 5100_u16.checked_add(index).ok_or(ControlError::Capacity)?;
    Ok(SocketAddr::from(([127, 0, 0, 1], port)))
}

fn interval(seconds: u32) -> Result<NonZeroU32, ControlError> {
    NonZeroU32::new(seconds).ok_or(ControlError::Invalid("Expected a positive peer wait"))
}

#[test]
fn success_releases_reservations_without_erasing_another_peers_retry_deadline(
) -> Result<(), ControlError> {
    let mut waits = PeerWaits::default();
    let primary = peer(0)?;
    let backup = peer(1)?;
    assert_eq!(waits.reserve(primary, 1100)?, Admission::Ready);
    waits.postpone(primary, interval(10)?, 1100)?;
    assert_eq!(waits.reserve(backup, 1200)?, Admission::Ready);
    waits.release_unused();
    assert_eq!(
        waits.reserve(primary, 3400)?,
        Admission::Wait { at: 11_100 }
    );
    assert_eq!(waits.reserve(backup, 3400)?, Admission::Ready);
    waits.postpone(backup, interval(20)?, 3500)?;
    waits.release_unused();
    assert_eq!(
        waits.reserve(primary, 11_099)?,
        Admission::Wait { at: 11_100 }
    );
    assert_eq!(
        waits.reserve(backup, 11_099)?,
        Admission::Wait { at: 23_500 }
    );
    assert_eq!(waits.reserve(primary, 11_100)?, Admission::Ready);
    Ok(())
}

#[test]
fn eight_waiting_peers_keep_their_deadlines_and_defer_a_ninth_until_the_earliest(
) -> Result<(), ControlError> {
    let mut waits = PeerWaits::default();
    for index in 0..8_u16 {
        let peer = peer(index)?;
        assert_eq!(waits.reserve(peer, 1000)?, Admission::Ready);
        waits.postpone(peer, interval(8 + u32::from(index))?, 1000)?;
    }
    let ninth = peer(8)?;
    assert_eq!(waits.reserve(ninth, 2000)?, Admission::Wait { at: 9000 });
    for index in 0..8_u16 {
        assert_eq!(
            waits.reserve(peer(index)?, 2000)?,
            Admission::Wait {
                at: 9000 + u64::from(index) * 1000
            }
        );
    }
    assert_eq!(waits.reserve(ninth, 8999)?, Admission::Wait { at: 9000 });
    assert_eq!(waits.reserve(ninth, 9000)?, Admission::Ready);
    for index in 1..8_u16 {
        assert_eq!(
            waits.reserve(peer(index)?, 9000)?,
            Admission::Wait {
                at: 9000 + u64::from(index) * 1000
            }
        );
    }
    assert_eq!(
        waits.reserve(peer(9)?, 9000)?,
        Admission::Wait { at: 10_000 }
    );
    Ok(())
}

#[test]
fn exact_expiry_releases_only_due_peers() -> Result<(), ControlError> {
    let mut waits = PeerWaits::default();
    let primary = peer(0)?;
    let backup = peer(1)?;
    assert_eq!(waits.reserve(primary, 1000)?, Admission::Ready);
    waits.postpone(primary, interval(10)?, 1000)?;
    assert_eq!(waits.reserve(backup, 1000)?, Admission::Ready);
    waits.postpone(backup, interval(20)?, 1000)?;
    assert_eq!(
        waits.reserve(primary, 10_999)?,
        Admission::Wait { at: 11_000 }
    );
    assert_eq!(waits.reserve(primary, 11_000)?, Admission::Ready);
    assert_eq!(
        waits.reserve(backup, 11_000)?,
        Admission::Wait { at: 21_000 }
    );
    waits.release_unused();
    assert_eq!(
        waits.reserve(backup, 20_999)?,
        Admission::Wait { at: 21_000 }
    );
    assert_eq!(waits.reserve(backup, 21_000)?, Admission::Ready);
    Ok(())
}

#[test]
fn deadline_overflow_does_not_replace_an_existing_wait() -> Result<(), ControlError> {
    let mut waits = PeerWaits::default();
    let primary = peer(0)?;
    let backup = peer(1)?;
    let start = u64::MAX - u64::from(u32::MAX) * 1000;
    assert_eq!(waits.reserve(primary, start)?, Admission::Ready);
    waits.postpone(primary, interval(u32::MAX)?, start)?;
    assert_eq!(waits.reserve(backup, start + 1)?, Admission::Ready);
    assert!(matches!(
        waits.postpone(backup, interval(u32::MAX)?, start + 1),
        Err(ControlError::Capacity)
    ));
    assert_eq!(
        waits.reserve(primary, start + 2)?,
        Admission::Wait { at: u64::MAX }
    );
    assert_eq!(waits.reserve(backup, start + 2)?, Admission::Ready);
    waits.release_unused();
    assert_eq!(
        waits.reserve(primary, start + 2)?,
        Admission::Wait { at: u64::MAX }
    );
    assert_eq!(waits.reserve(primary, u64::MAX)?, Admission::Ready);
    Ok(())
}

#[test]
fn reservations_without_deadlines_report_capacity_without_fabricating_a_wakeup(
) -> Result<(), ControlError> {
    let mut waits = PeerWaits::default();
    for index in 0..8_u16 {
        assert_eq!(waits.reserve(peer(index)?, 1000)?, Admission::Ready);
    }
    assert!(matches!(
        waits.reserve(peer(8)?, 1000),
        Err(ControlError::Capacity)
    ));
    for index in 0..8_u16 {
        assert_eq!(waits.reserve(peer(index)?, 1000)?, Admission::Ready);
    }
    waits.release_unused();
    assert_eq!(waits.reserve(peer(8)?, 1000)?, Admission::Ready);
    Ok(())
}
