use std::io;

use super::{format_default, format_millis, Timestamp};

mod fixtures;

fn assert_golden(golden: fixtures::Golden) -> io::Result<()> {
    let nanos = i32::try_from(golden.nanoseconds).map_err(io::Error::other)?;
    let timestamp = Timestamp::new(golden.seconds, nanos).map_err(io::Error::other)?;
    assert_eq!(format_default(timestamp), golden.default);
    assert_eq!(format_millis(timestamp), golden.millis);
    Ok(())
}

#[test]
fn whole_seconds_keep_offset_and_zero_milliseconds() -> io::Result<()> {
    assert_golden(fixtures::WHOLE_SECOND)
}

#[test]
fn millisecond_precision_keeps_three_digits() -> io::Result<()> {
    assert_golden(fixtures::MILLISECOND_ALIGNED)
}

#[test]
fn default_precision_preserves_trailing_microsecond_zeros() -> io::Result<()> {
    assert_golden(fixtures::MICROSECOND_TRAILING_ZEROS)
}

#[test]
fn microsecond_precision_keeps_six_digits() -> io::Result<()> {
    assert_golden(fixtures::MICROSECOND_ALIGNED)
}

#[test]
fn nanosecond_precision_keeps_nine_digits() -> io::Result<()> {
    assert_golden(fixtures::NANOSECOND_FRACTION)
}

#[test]
fn hundred_nanoseconds_preserve_leading_and_trailing_zeros() -> io::Result<()> {
    assert_golden(fixtures::HUNDRED_NANOSECONDS)
}

#[test]
fn milliseconds_truncate_the_last_nanosecond_of_a_second() -> io::Result<()> {
    assert_golden(fixtures::END_OF_SECOND)
}

#[test]
fn pre_epoch_milliseconds_follow_the_utc_calendar_second() -> io::Result<()> {
    assert_golden(fixtures::BEFORE_EPOCH)
}
