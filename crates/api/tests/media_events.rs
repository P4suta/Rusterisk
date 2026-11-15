use rusterisk_api::{MediaControlFormat, MediaEvent};

#[test]
fn websocket_media_events_preserve_reference_fields_in_both_formats(
) -> Result<(), serde_json::Error> {
    for (json, plain) in [
        (r#"{"event":"MEDIA_START","connection_id":"connection","channel":"WebSocket/INCOMING/channel","channel_id":"channel","format":"ulaw","optimal_frame_size":160,"ptime":20,"channel_variables":{}}"#, "MEDIA_START connection_id:connection channel:WebSocket/INCOMING/channel channel_id:channel format:ulaw optimal_frame_size:160 ptime:20"),
        (r##"{"event":"DTMF_END","channel_id":"channel","digit":"#"}"##, "DTMF_END digit:# channel_id:channel"),
        (r#"{"event":"MEDIA_XOFF","channel_id":"channel"}"#, "MEDIA_XOFF"),
        (r#"{"event":"MEDIA_XON","channel_id":"channel"}"#, "MEDIA_XON"),
        (r#"{"event":"QUEUE_DRAINED","channel_id":"channel"}"#, "QUEUE_DRAINED"),
        (r#"{"event":"ERROR","channel_id":"channel","error_text":"Invalid direction"}"#, "ERROR channel_id:channel error_text:Invalid direction"),
        (r#"{"event":"MEDIA_BUFFERING_COMPLETED","channel_id":"channel","correlation_id":"transfer"}"#, "MEDIA_BUFFERING_COMPLETED transfer"),
        (r#"{"event":"MEDIA_MARK_PROCESSED","channel_id":"channel","correlation_id":"position"}"#, "MEDIA_MARK_PROCESSED position"),
        (r#"{"event":"STATUS","channel_id":"channel","queue_length":3,"xon_level":800,"xoff_level":900,"queue_full":false,"bulk_media":true,"media_paused":false}"#, "STATUS channel_id:channel queue_length:3 xon_level:800 xoff_level:900 queue_full:false bulk_media:true media_paused:false"),
    ] {
        let event: MediaEvent = serde_json::from_str(json)?;
        assert_eq!(serde_json::to_value(&event)?, serde_json::from_str::<serde_json::Value>(json)?);
        assert_eq!(event.encode(MediaControlFormat::PlainText).map_err(serde::de::Error::custom)?, plain);
        assert_eq!(serde_json::from_str::<serde_json::Value>(&event.encode(MediaControlFormat::Json).map_err(serde::de::Error::custom)?)?, serde_json::from_str::<serde_json::Value>(json)?);
    }
    Ok(())
}

#[test]
fn media_formats_describe_linear_frames_and_packet_passthrough() -> Result<(), serde_json::Error> {
    for (text, rate, size) in [
        ("ulaw", 8000, 160),
        ("alaw", 8000, 160),
        ("slin", 8000, 320),
        ("slin12", 12000, 480),
        ("slin16", 16000, 640),
        ("slin24", 24000, 960),
        ("slin32", 32000, 1280),
        ("slin44", 44100, 1764),
        ("slin48", 48000, 1920),
        ("opus", 48000, 0),
    ] {
        let format: rusterisk_api::MediaFormat = serde_json::from_value(text.into())?;
        assert_eq!(format.rate(), rate);
        assert_eq!(format.optimal_frame_size(), size);
        assert_eq!(format.as_str(), text);
    }
    assert!(serde_json::from_value::<rusterisk_api::MediaFormat>("unknown".into()).is_err());
    Ok(())
}
