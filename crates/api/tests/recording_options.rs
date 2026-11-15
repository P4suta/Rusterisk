use rusterisk_api::{RecordingIfExists, RecordingOptions, RecordingSeconds, RecordingTermination};

#[test]
fn recording_options_validate_integer_ranges_and_reference_termination_values(
) -> Result<(), serde_json::Error> {
    for (text, expected) in [
        ("none", RecordingTermination::None),
        ("any", RecordingTermination::Any),
        ("*", RecordingTermination::Star),
        ("#", RecordingTermination::Hash),
    ] {
        assert_eq!(RecordingTermination::try_from(text), Ok(expected));
        assert_eq!(
            RecordingTermination::try_from(text.to_ascii_uppercase().as_str()),
            Ok(expected)
        );
        assert_eq!(serde_json::to_value(expected)?, text);
    }
    assert_eq!(
        RecordingTermination::try_from(""),
        Ok(RecordingTermination::None)
    );
    for invalid in ["0", "A", "##", "all", "＊"] {
        assert!(RecordingTermination::try_from(invalid).is_err());
    }
    for seconds in [0, 1, i32::MAX as u32] {
        let value = RecordingSeconds::try_from(seconds).map_err(serde::de::Error::custom)?;
        assert_eq!(value.seconds(), seconds);
        assert_eq!(serde_json::to_value(value)?, seconds);
        assert_eq!(
            serde_json::from_value::<RecordingSeconds>(seconds.into())?,
            value
        );
    }
    for value in [
        serde_json::json!(-1),
        serde_json::json!(2147483648_u32),
        serde_json::json!(1.5),
        serde_json::json!("1"),
    ] {
        assert!(serde_json::from_value::<RecordingSeconds>(value).is_err());
    }
    let options: RecordingOptions = serde_json::from_value(
        serde_json::json!({"ifExists": "append", "terminateOn": "#", "maxDurationSeconds": 60, "maxSilenceSeconds": 2, "beep": true}),
    )?;
    assert_eq!(options.if_exists, RecordingIfExists::Append);
    assert_eq!(options.max_duration_seconds.seconds(), 60);
    assert_eq!(options.max_silence_seconds.seconds(), 2);
    assert_eq!(options.terminate_on, RecordingTermination::Hash);
    assert!(options.beep);
    assert_eq!(
        serde_json::from_value::<RecordingOptions>(serde_json::json!({}))?,
        RecordingOptions::default()
    );
    assert!(
        serde_json::from_value::<RecordingOptions>(serde_json::json!({"duration": 60})).is_err()
    );
    let schema = schemars::schema_for!(RecordingSeconds).to_value();
    assert_eq!(schema["type"], "integer");
    assert_eq!(schema["minimum"], 0);
    assert_eq!(schema["maximum"], i32::MAX);
    Ok(())
}
