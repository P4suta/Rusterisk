use rusterisk_sip::{SipStreamDecoder, StreamFramingError, StreamItem, MAX_MESSAGE};

fn message(length: &str, body: &[u8]) -> Vec<u8> {
    let mut bytes = format!("OPTIONS sip:phone@localhost SIP/2.0\r\nVia: SIP/2.0/TCP localhost;branch=z9hG4bK-stream\r\nFrom: <sip:phone@localhost>;tag=source\r\nTo: <sip:phone@localhost>\r\nCall-ID: stream-test\r\nCSeq: 1 OPTIONS\r\nMax-Forwards: 70\r\n{length}\r\n").into_bytes();
    bytes.extend_from_slice(body);
    bytes
}

#[test]
fn fragmented_headers_and_binary_bodies_resume_at_every_byte_boundary(
) -> Result<(), StreamFramingError> {
    let wire = message("Content-Length: 9\r\n", b"a\0\r\n\r\n\xffbc");
    for boundary in 0..wire.len() {
        let mut decoder = SipStreamDecoder::new();
        assert_eq!(decoder.push(&wire[..boundary])?, (boundary, None));
        assert_eq!(decoder.push(&[])?, (0, None));
        let (consumed, item) = decoder.push(&wire[boundary..])?;
        assert_eq!(consumed, wire.len() - boundary);
        assert_eq!(item, Some(StreamItem::Message(wire.clone())));
        assert!(decoder.is_empty());
    }
    Ok(())
}

#[test]
fn combined_messages_and_leading_crlf_pairs_preserve_the_consumed_offset(
) -> Result<(), StreamFramingError> {
    let first = message("l: 0\r\n", b"");
    let second = message("cOnTeNt-LeNgTh \t: \r\n\t3 \t\r\n", b"end");
    let bytes: Vec<u8> = b"\r\n\r\n"
        .iter()
        .chain(&first)
        .chain(&second)
        .copied()
        .collect();
    let mut decoder = SipStreamDecoder::new();
    assert_eq!(decoder.push(&bytes)?, (2, Some(StreamItem::Crlf)));
    assert_eq!(decoder.push(&bytes[2..])?, (2, Some(StreamItem::Crlf)));
    assert_eq!(
        decoder.push(&bytes[4..])?,
        (first.len(), Some(StreamItem::Message(first.clone())))
    );
    assert_eq!(
        decoder.push(&bytes[4 + first.len()..])?,
        (second.len(), Some(StreamItem::Message(second)))
    );
    decoder.finish()?;
    assert_eq!(decoder.push(&first), Err(StreamFramingError::Closed));
    Ok(())
}

#[test]
fn invalid_or_ambiguous_lengths_close_the_decoder_without_accepting_later_messages() {
    for (header, error) in [
        ("", StreamFramingError::MissingLength),
        ("Content-Length: \r\n", StreamFramingError::InvalidLength),
        ("Content-Length: +1\r\n", StreamFramingError::InvalidLength),
        ("Content-Length: -1\r\n", StreamFramingError::InvalidLength),
        ("Content-Length: 1x\r\n", StreamFramingError::InvalidLength),
        ("Content-Length: 1 2\r\n", StreamFramingError::InvalidLength),
        (
            "Content-Length: 1\r\n 2\r\n",
            StreamFramingError::InvalidLength,
        ),
        (
            "Content-Length: 0\r\nContent-Length: 0\r\n",
            StreamFramingError::DuplicateLength,
        ),
        (
            "Content-Length: 0\r\nl: 0\r\n",
            StreamFramingError::DuplicateLength,
        ),
        (
            "Content-Length: 999999999999999999999999999\r\n",
            StreamFramingError::Capacity,
        ),
    ] {
        let mut decoder = SipStreamDecoder::new();
        assert_eq!(
            decoder.push(&message(header, b"")),
            Err(error),
            "{header:?}"
        );
        assert_eq!(
            decoder.push(&message("Content-Length: 0\r\n", b"")),
            Err(StreamFramingError::Closed)
        );
    }
}

#[test]
fn declared_total_sizes_obey_the_packet_limit_before_body_allocation(
) -> Result<(), StreamFramingError> {
    let prefix = message("Content-Length: 16000\r\n", b"");
    let mut decoder = SipStreamDecoder::new();
    assert_eq!(decoder.push(&prefix)?, (prefix.len(), None));
    let body = vec![0; 16000];
    let (consumed, result) = decoder.push(&body)?;
    assert_eq!(consumed, body.len());
    let Some(StreamItem::Message(accepted)) = result else {
        return Err(StreamFramingError::Incomplete);
    };
    assert_eq!(accepted.len(), prefix.len() + body.len());
    let exact_header = format!(
        "X:{}\r\nContent-Length: 0\r\n",
        "a".repeat(MAX_MESSAGE - message("X:\r\nContent-Length: 0\r\n", b"").len())
    );
    let exact = message(&exact_header, b"");
    assert_eq!(exact.len(), MAX_MESSAGE);
    assert_eq!(
        decoder.push(&exact)?,
        (MAX_MESSAGE, Some(StreamItem::Message(exact)))
    );
    for wire in [
        message(&format!("Content-Length: {MAX_MESSAGE}\r\n"), b""),
        vec![b'A'; MAX_MESSAGE + 1],
    ] {
        let mut decoder = SipStreamDecoder::new();
        assert_eq!(decoder.push(&wire), Err(StreamFramingError::Capacity));
        assert_eq!(decoder.push(b""), Err(StreamFramingError::Closed));
    }
    Ok(())
}

#[test]
fn malformed_line_endings_and_incomplete_shutdown_are_terminal() -> Result<(), StreamFramingError> {
    for wire in [
        b"OPTIONS sip:a SIP/2.0\n".as_slice(),
        b"OPTIONS sip:a SIP/2.0\rX",
        b"\n",
        b"\rX",
    ] {
        let mut decoder = SipStreamDecoder::new();
        assert_eq!(decoder.push(wire), Err(StreamFramingError::InvalidLine));
        assert_eq!(decoder.push(b""), Err(StreamFramingError::Closed));
    }
    for wire in [
        b"\r".as_slice(),
        message("Content-Length: 4\r\n", b"ab").as_slice(),
    ] {
        let mut decoder = SipStreamDecoder::new();
        assert_eq!(decoder.push(wire)?, (wire.len(), None));
        assert_eq!(decoder.finish(), Err(StreamFramingError::Incomplete));
        assert_eq!(decoder.push(b"cd"), Err(StreamFramingError::Closed));
    }
    Ok(())
}

#[test]
fn folded_lines_require_a_prior_header_and_decimal_lengths_use_only_spaces_and_tabs() {
    let orphan = b"OPTIONS sip:phone@localhost SIP/2.0\r\n orphan\r\nContent-Length: 0\r\n\r\n";
    let mut decoder = SipStreamDecoder::new();
    assert_eq!(decoder.push(orphan), Err(StreamFramingError::InvalidLine));
    for header in [
        "Content-Length: \u{b}0\r\n",
        "Content-Length: 0\u{c}\r\n",
        "Content-Length: 0,0\r\n",
    ] {
        let mut decoder = SipStreamDecoder::new();
        assert_eq!(
            decoder.push(&message(header, b"")),
            Err(StreamFramingError::InvalidLength)
        );
    }
}

#[test]
fn bytewise_keepalive_pairs_and_pipelined_messages_preserve_all_octets(
) -> Result<(), StreamFramingError> {
    let first = message("Content-Length: 0\r\n", b"");
    let second = message("l: 3\r\n", b"end");
    let bytes: Vec<u8> = b"\r\n\r\n"
        .iter()
        .chain(&first)
        .chain(b"\r\n")
        .chain(&second)
        .copied()
        .collect();
    let mut decoder = SipStreamDecoder::new();
    let mut received = Vec::with_capacity(5);
    for byte in bytes {
        let (consumed, item) = decoder.push(&[byte])?;
        assert_eq!(consumed, 1);
        if let Some(item) = item {
            received.push(item);
        }
    }
    assert_eq!(
        received,
        [
            StreamItem::Crlf,
            StreamItem::Crlf,
            StreamItem::Message(first),
            StreamItem::Crlf,
            StreamItem::Message(second)
        ]
    );
    decoder.finish()?;
    Ok(())
}

#[test]
fn complete_owned_messages_pass_directly_to_the_existing_engine(
) -> Result<(), rusterisk_sip::SipError> {
    let config = rusterisk_config::Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=test-password\n",
        "",
        "",
        "",
    )?;
    let mut engine = rusterisk_sip::Engine::new(config, [7; 32]);
    let wire = message("Content-Length: 0\r\n", b"");
    let mut decoder = SipStreamDecoder::new();
    let (_, item) = decoder
        .push(&wire)
        .map_err(|error| rusterisk_sip::SipError::Io(std::io::Error::other(error)))?;
    let Some(StreamItem::Message(bytes)) = item else {
        return Err(rusterisk_sip::SipError::InvalidPacket);
    };
    let response = engine.receive(&bytes, "127.0.0.1:5061".parse()?, 0)?;
    assert_eq!(response.datagrams.len(), 1);
    assert!(response.datagrams[0]
        .bytes
        .starts_with(b"SIP/2.0 200 OK\r\n"));
    assert!(response.events.is_empty());
    Ok(())
}
