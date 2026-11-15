use rusterisk_sip::{validate_websocket_message, StreamFramingError, MAX_MESSAGE};

fn message(headers: &str, body: &[u8]) -> Vec<u8> {
    let mut bytes = format!("OPTIONS sip:localhost SIP/2.0\r\nVia: SIP/2.0/WS browser.invalid;branch=z9hG4bK-ws\r\nFrom: <sip:phone@localhost>;tag=phone\r\nTo: <sip:server@localhost>\r\nCall-ID: websocket\r\nCSeq: 1 OPTIONS\r\n{headers}\r\n").into_bytes();
    bytes.extend_from_slice(body);
    bytes
}

#[test]
fn websocket_boundaries_accept_optional_lengths_and_binary_bodies() -> Result<(), StreamFramingError>
{
    for headers in [
        "",
        "Content-Length: 0\r\n",
        "l: 0\r\n",
        "Content-Length:\r\n 0\r\n",
    ] {
        validate_websocket_message(&message(headers, b""))?;
    }
    let body = [0, 255, 128, b'\r', b'\n'];
    for headers in [
        "Content-Type: application/octet-stream\r\n",
        "Content-Type: application/octet-stream\r\nContent-Length: 5\r\n",
    ] {
        validate_websocket_message(&message(headers, &body))?;
    }
    Ok(())
}

#[test]
fn websocket_boundaries_reject_joined_partial_and_ambiguous_messages() {
    let complete = message("Content-Length: 0\r\n", b"");
    let mut joined = complete.clone();
    joined.extend_from_slice(&complete);
    for bytes in [
        joined,
        complete[..complete.len() - 1].to_vec(),
        message("Content-Length: 1\r\n", b""),
        message("Content-Length: 0\r\n", b"x"),
        message("Content-Length: 0\r\nl: 0\r\n", b""),
        message("Content-Length: +0\r\n", b""),
        b"OPTIONS sip:localhost SIP/2.0\r\n orphaned\r\n\r\n".to_vec(),
        message("X-Malformed\r\n", b""),
        b"\r\n".to_vec(),
        b"\r\n\r\n".to_vec(),
    ] {
        assert!(validate_websocket_message(&bytes).is_err(), "{bytes:?}");
    }
    let malformed: Vec<u8> = complete
        .iter()
        .map(|byte| if *byte == b'\r' { b'\n' } else { *byte })
        .collect();
    assert!(validate_websocket_message(&malformed).is_err());
}

#[test]
fn websocket_messages_obey_the_same_owned_message_limit() -> Result<(), StreamFramingError> {
    let prefix = message("Content-Type: application/octet-stream\r\n", b"");
    let mut bytes = prefix;
    bytes.resize(MAX_MESSAGE, 0);
    validate_websocket_message(&bytes)?;
    bytes.push(0);
    assert_eq!(
        validate_websocket_message(&bytes),
        Err(StreamFramingError::Capacity)
    );
    Ok(())
}
