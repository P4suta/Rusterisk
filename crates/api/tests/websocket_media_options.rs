use rusterisk_api::{MediaControlFormat, MediaDirection, MediaFormat, WebSocketMediaOptions};

#[test]
fn media_options_encode_reference_dial_options_without_string_parameters() {
    let mut options = WebSocketMediaOptions::new(MediaFormat::Pcmu);
    assert_eq!(options.control_format, MediaControlFormat::PlainText);
    assert_eq!(options.direction, MediaDirection::Both);
    assert!(options.auto_answer);
    assert_eq!(options.transport_data(), "f(plain-text)");
    options.control_format = MediaControlFormat::Json;
    options.auto_answer = false;
    options.direction = MediaDirection::Out;
    assert_eq!(options.transport_data(), "f(json)n");
    assert_eq!(options.direction.as_str(), "out");
}
