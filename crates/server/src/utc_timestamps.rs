use jiff::{tz::Offset, Timestamp};

#[cfg(test)]
mod tests;

pub(crate) fn now_default() -> String {
    format_default(Timestamp::now())
}

pub(crate) fn now_millis() -> String {
    format_millis(Timestamp::now())
}

fn format_default(timestamp: Timestamp) -> String {
    let nanos = timestamp.subsec_nanosecond().unsigned_abs();
    let precision = if nanos == 0 {
        0
    } else if nanos.is_multiple_of(1_000_000) {
        3
    } else if nanos.is_multiple_of(1_000) {
        6
    } else {
        9
    };
    let display = timestamp.display_with_offset(Offset::UTC);
    format!("{display:.precision$}")
}

fn format_millis(timestamp: Timestamp) -> String {
    format!("{timestamp:.3}")
}
