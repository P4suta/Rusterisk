use rusterisk_api::{PlaybackMilliseconds, PlaybackOptions};

#[test]
fn playback_options_validate_reference_integer_ranges_and_zero_step_defaults(
) -> Result<(), serde_json::Error> {
    let default = PlaybackOptions::default();
    assert_eq!(default.offset_ms.milliseconds(), 0);
    assert_eq!(default.step_ms(), 3000);
    assert_eq!(
        serde_json::from_value::<PlaybackOptions>(serde_json::json!({}))?,
        default
    );
    for milliseconds in [0, 1, 3000, i32::MAX as u32] {
        let value =
            PlaybackMilliseconds::try_from(milliseconds).map_err(serde::de::Error::custom)?;
        assert_eq!(value.milliseconds(), milliseconds);
        assert_eq!(serde_json::to_value(value)?, milliseconds);
        assert_eq!(
            serde_json::from_value::<PlaybackMilliseconds>(milliseconds.into())?,
            value
        );
        let options: PlaybackOptions = serde_json::from_value(
            serde_json::json!({"offsetms": milliseconds, "skipms": milliseconds}),
        )?;
        assert_eq!(options.offset_ms, value);
        assert_eq!(options.skip_ms, value);
        assert_eq!(
            options.step_ms(),
            if milliseconds == 0 {
                3000
            } else {
                milliseconds
            }
        );
    }
    for value in [
        serde_json::json!(-1),
        serde_json::json!(2147483648_u32),
        serde_json::json!(u32::MAX),
        serde_json::json!(1.5),
        serde_json::json!("1"),
        serde_json::Value::Null,
    ] {
        assert!(serde_json::from_value::<PlaybackMilliseconds>(value.clone()).is_err());
        assert!(serde_json::from_value::<PlaybackOptions>(
            serde_json::json!({"offsetms": value.clone()})
        )
        .is_err());
        assert!(
            serde_json::from_value::<PlaybackOptions>(serde_json::json!({"skipms": value}))
                .is_err()
        );
    }
    assert!(PlaybackMilliseconds::try_from(u32::MAX).is_err());
    assert!(serde_json::from_value::<PlaybackOptions>(serde_json::json!({"offset": 1})).is_err());
    let schema = schemars::schema_for!(PlaybackMilliseconds).to_value();
    assert_eq!(schema["type"], "integer");
    assert_eq!(schema["minimum"], 0);
    assert_eq!(schema["maximum"], i32::MAX);
    Ok(())
}
