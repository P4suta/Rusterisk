use rusterisk_api::{MediaCommand, MediaControlFormat};

#[test]
fn media_commands_roundtrip_both_reference_control_formats() -> Result<(), serde_json::Error> {
    for (plain, json) in [
        ("ANSWER", r#"{"command":"ANSWER"}"#),
        ("HANGUP", r#"{"command":"HANGUP"}"#),
        (
            "START_MEDIA_BUFFERING",
            r#"{"command":"START_MEDIA_BUFFERING"}"#,
        ),
        (
            "STOP_MEDIA_BUFFERING transfer",
            r#"{"command":"STOP_MEDIA_BUFFERING","correlation_id":"transfer"}"#,
        ),
        ("FLUSH_MEDIA", r#"{"command":"FLUSH_MEDIA"}"#),
        ("PAUSE_MEDIA", r#"{"command":"PAUSE_MEDIA"}"#),
        ("CONTINUE_MEDIA", r#"{"command":"CONTINUE_MEDIA"}"#),
        (
            "MARK_MEDIA position",
            r#"{"command":"MARK_MEDIA","correlation_id":"position"}"#,
        ),
        ("GET_STATUS", r#"{"command":"GET_STATUS"}"#),
        (
            "REPORT_QUEUE_DRAINED",
            r#"{"command":"REPORT_QUEUE_DRAINED"}"#,
        ),
        (
            "SET_MEDIA_DIRECTION in",
            r#"{"command":"SET_MEDIA_DIRECTION","direction":"in"}"#,
        ),
        (
            "SET_MEDIA_DIRECTION out",
            r#"{"command":"SET_MEDIA_DIRECTION","direction":"out"}"#,
        ),
        (
            "SET_MEDIA_DIRECTION both",
            r#"{"command":"SET_MEDIA_DIRECTION","direction":"both"}"#,
        ),
    ] {
        let expected: serde_json::Value = serde_json::from_str(json)?;
        let command = MediaCommand::parse(plain, MediaControlFormat::PlainText)
            .map_err(serde::de::Error::custom)?;
        assert_eq!(serde_json::to_value(&command)?, expected);
        assert_eq!(
            MediaCommand::parse(json, MediaControlFormat::Json)
                .map_err(serde::de::Error::custom)?,
            command
        );
        assert_eq!(
            command
                .encode(MediaControlFormat::PlainText)
                .map_err(serde::de::Error::custom)?,
            plain
        );
    }
    for invalid in [
        "",
        "answer",
        "SET_MEDIA_DIRECTION",
        "SET_MEDIA_DIRECTION sideways",
        "UNKNOWN",
        &"x".repeat(129),
    ] {
        assert!(MediaCommand::parse(invalid, MediaControlFormat::PlainText).is_err());
    }
    assert!(MediaCommand::parse(
        r#"{"command":"SET_MEDIA_DIRECTION","direction":"sideways"}"#,
        MediaControlFormat::Json
    )
    .is_err());
    assert_eq!(
        MediaCommand::parse(" \tANSWER extra \r\n", MediaControlFormat::PlainText)
            .map_err(serde::de::Error::custom)?,
        MediaCommand::Answer
    );
    let long_json = format!("{{\"command\":\"ANSWER\"}}{}", " ".repeat(128));
    assert!(MediaCommand::parse(&long_json, MediaControlFormat::Json).is_err());
    let long_marker: MediaCommand = serde_json::from_str(&format!(
        "{{\"command\":\"MARK_MEDIA\",\"correlation_id\":\"{}\"}}",
        "x".repeat(100)
    ))?;
    assert!(long_marker.encode(MediaControlFormat::Json).is_err());
    Ok(())
}
