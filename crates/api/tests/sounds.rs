use rusterisk_api::{openapi, Sound};

#[test]
fn sound_descriptions_omit_unavailable_transcripts() -> Result<(), serde_json::Error> {
    for source in [
        r#"{"id":"digits/1","formats":[{"language":"en","format":"slin"}]}"#,
        r#"{"id":"digits/1","text":"one","formats":[{"language":"en","format":"slin"}]}"#,
    ] {
        let value: serde_json::Value = serde_json::from_str(source)?;
        let sound: Sound = serde_json::from_value(value.clone())?;
        assert_eq!(serde_json::to_value(sound)?, value);
    }
    let document = openapi();
    let required = document["components"]["schemas"]["Sound"]["required"].as_array();
    assert!(required.is_some());
    if let Some(required) = required {
        assert!(!required.iter().any(|field| field == "text"));
        assert!(required.iter().any(|field| field == "id"));
        assert!(required.iter().any(|field| field == "formats"));
    }
    Ok(())
}
