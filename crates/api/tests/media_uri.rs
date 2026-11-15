use rusterisk_api::{ChannelId, InvalidMediaUri, MediaUri};

#[test]
fn media_uris_keep_their_scheme_and_validated_resource_name() -> Result<(), InvalidMediaUri> {
    for (text, expected) in [
        (
            "sound:welcome",
            MediaUri::Sound(
                ChannelId::try_from("welcome".to_owned())
                    .map_err(|_| InvalidMediaUri::Name)?
                    .into(),
            ),
        ),
        (
            "recording:call-123",
            MediaUri::Recording(
                ChannelId::try_from("call-123".to_owned()).map_err(|_| InvalidMediaUri::Name)?,
            ),
        ),
    ] {
        let parsed = MediaUri::try_from(text)?;
        assert_eq!(parsed, expected);
        assert_eq!(parsed.to_string(), text);
    }
    Ok(())
}

#[test]
fn invalid_names_and_unsupported_schemes_have_distinct_failures() {
    assert_eq!(MediaUri::try_from("welcome"), Err(InvalidMediaUri::Syntax));
    for value in [
        "sound:",
        "sound:../secret",
        "recording:a/b",
        "sound:bad?query",
    ] {
        assert_eq!(MediaUri::try_from(value), Err(InvalidMediaUri::Name));
    }
    assert_eq!(
        MediaUri::try_from("https://example.test/audio.wav"),
        Err(InvalidMediaUri::Unsupported)
    );
    assert_eq!(
        MediaUri::try_from(format!("sound:{}", "a".repeat(129)).as_str()),
        Err(InvalidMediaUri::Name)
    );
}
