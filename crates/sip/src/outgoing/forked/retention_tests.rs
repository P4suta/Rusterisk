use super::{Forks, Retention, CAPACITY};
use crate::{SipError, MAX_MESSAGE};

const BUFFER_CAPACITY: usize = 2 * 1024 * 1024;

fn packet(capacity: usize) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(capacity);
    bytes.extend_from_slice(b"ACK");
    bytes
}

fn retained(
    forks: &mut Forks,
    fingerprint: [u8; 32],
    until: u64,
    ack: Vec<u8>,
) -> Result<(), SipError> {
    match forks.retain(fingerprint, until, ack) {
        Retention::Stored(_) => Ok(()),
        Retention::Refused(_) => Err(SipError::Capacity),
    }
}

fn refused(
    forks: &mut Forks,
    fingerprint: [u8; 32],
    until: u64,
    ack: Vec<u8>,
) -> Result<Vec<u8>, SipError> {
    match forks.retain(fingerprint, until, ack) {
        Retention::Refused(ack) => Ok(ack),
        Retention::Stored(_) => Err(SipError::Io(std::io::Error::other(
            "An ACK allocation was admitted beyond its retention boundary",
        ))),
    }
}

#[test]
fn shared_teardown_metadata_stays_within_its_engine_memory_budget() {
    assert!(std::mem::size_of::<Forks>() <= 16 * 1024);
}

#[test]
fn retained_buffers_charge_spare_capacity_and_refuse_an_oversized_allocation(
) -> Result<(), SipError> {
    let mut forks = Forks::default();
    let oversized = packet(BUFFER_CAPACITY + 1);
    let capacity = oversized.capacity();
    assert!(oversized.len() <= MAX_MESSAGE);
    assert!(capacity > BUFFER_CAPACITY);
    let ack = refused(&mut forks, [1; 32], 10, oversized)?;
    assert_eq!(ack.capacity(), capacity);
    assert_eq!(forks.retained_bytes, 0);
    assert!(forks.entries.iter().all(Option::is_none));
    Ok(())
}

#[test]
fn aggregate_buffer_refusal_preserves_entries_and_expiry_returns_their_capacity(
) -> Result<(), SipError> {
    let mut forks = Forks::default();
    let first = packet(BUFFER_CAPACITY / 2 + 1);
    let first_capacity = first.capacity();
    let second = packet(BUFFER_CAPACITY / 2 + 1);
    let second_capacity = second.capacity();
    assert!(first_capacity <= BUFFER_CAPACITY && second_capacity <= BUFFER_CAPACITY);
    assert!(first_capacity + second_capacity > BUFFER_CAPACITY);
    retained(&mut forks, [1; 32], 10, first)?;
    let second = refused(&mut forks, [2; 32], 20, second)?;
    assert_eq!(forks.retained_bytes, first_capacity);
    assert!(forks.retained([1; 32]).is_some());
    assert!(forks.retained([2; 32]).is_none());
    let third = packet(3);
    let third_capacity = third.capacity();
    retained(&mut forks, [3; 32], 20, third)?;
    forks.expire(9)?;
    assert_eq!(forks.retained_bytes, first_capacity + third_capacity);
    forks.expire(10)?;
    assert_eq!(forks.retained_bytes, third_capacity);
    assert!(forks.retained([1; 32]).is_none());
    retained(&mut forks, [2; 32], 20, second)?;
    assert_eq!(forks.retained_bytes, second_capacity + third_capacity);
    forks.expire(20)?;
    assert_eq!(forks.retained_bytes, 0);
    assert!(forks.entries.iter().all(Option::is_none));
    Ok(())
}

#[test]
fn pending_acknowledgments_share_the_count_bound_without_losing_byte_accounting(
) -> Result<(), SipError> {
    let mut forks = Forks::default();
    let mut charged = 0;
    for fingerprint in 0..CAPACITY {
        let ack = packet(3);
        charged += ack.capacity();
        let fingerprint = u8::try_from(fingerprint).map_err(|_| SipError::Capacity)?;
        retained(&mut forks, [fingerprint; 32], 10, ack)?;
    }
    let ack = refused(&mut forks, [255; 32], 10, packet(3))?;
    assert_eq!(ack.as_slice(), b"ACK");
    assert_eq!(forks.retained_bytes, charged);
    forks.expire(10)?;
    assert_eq!(forks.retained_bytes, 0);
    retained(&mut forks, [255; 32], 20, ack)?;
    assert!(forks.retained([255; 32]).is_some());
    Ok(())
}
