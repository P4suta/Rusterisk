#[derive(Clone, Copy, Debug)]
pub(super) struct Golden {
    pub(super) seconds: i64,
    pub(super) nanoseconds: u32,
    pub(super) default: &'static str,
    pub(super) millis: &'static str,
}

pub(super) const WHOLE_SECOND: Golden = Golden {
    seconds: 0,
    nanoseconds: 0,
    default: "1970-01-01T00:00:00+00:00",
    millis: "1970-01-01T00:00:00.000Z",
};

pub(super) const MILLISECOND_ALIGNED: Golden = Golden {
    seconds: 0,
    nanoseconds: 100_000_000,
    default: "1970-01-01T00:00:00.100+00:00",
    millis: "1970-01-01T00:00:00.100Z",
};

pub(super) const MICROSECOND_TRAILING_ZEROS: Golden = Golden {
    seconds: 0,
    nanoseconds: 123_400_000,
    default: "1970-01-01T00:00:00.123400+00:00",
    millis: "1970-01-01T00:00:00.123Z",
};

pub(super) const MICROSECOND_ALIGNED: Golden = Golden {
    seconds: 0,
    nanoseconds: 123_456_000,
    default: "1970-01-01T00:00:00.123456+00:00",
    millis: "1970-01-01T00:00:00.123Z",
};

pub(super) const NANOSECOND_FRACTION: Golden = Golden {
    seconds: 0,
    nanoseconds: 123_456_789,
    default: "1970-01-01T00:00:00.123456789+00:00",
    millis: "1970-01-01T00:00:00.123Z",
};

pub(super) const HUNDRED_NANOSECONDS: Golden = Golden {
    seconds: 0,
    nanoseconds: 100,
    default: "1970-01-01T00:00:00.000000100+00:00",
    millis: "1970-01-01T00:00:00.000Z",
};

pub(super) const END_OF_SECOND: Golden = Golden {
    seconds: 0,
    nanoseconds: 999_999_999,
    default: "1970-01-01T00:00:00.999999999+00:00",
    millis: "1970-01-01T00:00:00.999Z",
};

pub(super) const BEFORE_EPOCH: Golden = Golden {
    seconds: -1,
    nanoseconds: 999_999_999,
    default: "1969-12-31T23:59:59.999999999+00:00",
    millis: "1969-12-31T23:59:59.999Z",
};
