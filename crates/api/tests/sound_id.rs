use rusterisk_api::{InvalidMediaUri, MediaUri, SoundId};

#[test]
fn sound_names_accept_bounded_relative_components_without_expanding_channel_identifiers(
) -> Result<(), InvalidMediaUri> {
    for name in [
        "welcome",
        "digits/1",
        "en_GB/digits/one.two",
        "a/b/c/d/e/f/g/h",
    ] {
        let sound = SoundId::try_from(name.to_owned()).map_err(|_| InvalidMediaUri::Name)?;
        assert_eq!(sound.as_str(), name);
        let uri = MediaUri::try_from(format!("sound:{name}").as_str())?;
        assert_eq!(uri, MediaUri::Sound(sound));
        assert_eq!(uri.to_string(), format!("sound:{name}"));
        assert!(rusterisk_api::ChannelId::try_from(name.to_owned()).is_ok() == !name.contains('/'));
    }
    Ok(())
}

#[test]
fn sound_names_refuse_traversal_and_cross_platform_or_encoded_separators() {
    for name in [
        "",
        ".",
        "..",
        "/welcome",
        "a/",
        "a//b",
        "../a",
        "a/../b",
        "a/./b",
        "a\\b",
        "C:/a",
        "%2e%2e/secret",
        "a%2fb",
        "a?query",
        "a#fragment",
        "a\0b",
        "a/b/c/d/e/f/g/h/i",
    ] {
        assert!(SoundId::try_from(name.to_owned()).is_err(), "{name:?}");
        assert!(MediaUri::try_from(format!("sound:{name}").as_str()).is_err());
    }
    assert!(SoundId::try_from("a".repeat(128)).is_ok());
    assert!(SoundId::try_from("a".repeat(129)).is_err());
    assert!(MediaUri::try_from("recording:digits/1").is_err());
    assert!(serde_json::from_str::<SoundId>("\"../secret\"").is_err());
}
