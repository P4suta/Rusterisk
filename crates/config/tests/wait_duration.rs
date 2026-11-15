use rusterisk_config::WaitDuration;

#[test]
fn wait_durations_convert_decimal_units_to_exact_truncated_milliseconds(
) -> Result<(), rusterisk_config::InvalidWaitDuration> {
    for (source, expected) in [
        ("0", 0),
        (".5", 500),
        ("1.001", 1001),
        ("0.0009", 0),
        ("1e-3", 1),
        ("1E1", 10000),
        ("1.5m", 90000),
        (".25h", 900000),
        ("1.2MS", 1),
        (" +0.1S ", 100),
        ("2147483.647", 2147483647),
    ] {
        let parsed = WaitDuration::parse(source)?;
        assert_eq!(parsed.milliseconds(), expected, "{source}");
        assert_eq!(parsed.argument(), source);
    }
    Ok(())
}

#[test]
fn wait_durations_reject_non_finite_negative_ambiguous_and_out_of_range_inputs() {
    for source in [
        "",
        " ",
        "-1",
        "NaN",
        "inf",
        "1e",
        "1e100",
        "1..5",
        "1 seconds",
        "1s trailing",
        "1msx",
        "2147483.648",
        "1,2",
        "1\n",
        "0x1p0",
    ] {
        assert!(WaitDuration::parse(source).is_err(), "{source:?}");
    }
    assert!(WaitDuration::parse(&"0".repeat(65)).is_err());
}
