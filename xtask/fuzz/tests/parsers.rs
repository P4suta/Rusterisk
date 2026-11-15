use rusterisk_input_fuzz::{controls, settings};

#[test]
fn maintained_corpora_have_valid_and_boundary_inputs_for_every_target() {
    use rusterisk_input_fuzz::{Target, MAX_INPUT};
    for target in Target::ALL {
        let seeds = target.seeds();
        assert!(seeds.len() >= 3);
        assert!(seeds.iter().any(|seed| seed.valid));
        assert!(seeds.iter().any(|seed| !seed.valid));
        for seed in seeds {
            assert!(!seed.name.is_empty());
            assert!(!seed.bytes.is_empty());
            assert!(seed.bytes.len() <= MAX_INPUT);
            let outcome = match target {
                Target::Settings => settings(seed.bytes),
                Target::Controls => controls(seed.bytes),
                #[cfg(feature = "signaling")]
                Target::Sip => rusterisk_input_fuzz::sip(seed.bytes),
                #[cfg(not(feature = "signaling"))]
                Target::Sip => continue,
                #[cfg(feature = "media")]
                Target::Media => rusterisk_input_fuzz::media(seed.bytes),
                #[cfg(not(feature = "media"))]
                Target::Media => continue,
            };
            assert!(
                outcome.is_ok(),
                "{}: {}: {outcome:?}",
                target.name(),
                seed.name
            );
            if seed.valid {
                assert!(matches!(outcome, Ok(count) if count > 0));
            }
        }
    }
}

#[test]
fn settings_seeds_exercise_configuration_and_reject_malformed_inputs() {
    for source in [
        b"[general]\nbindaddr=127.0.0.1\n".as_slice(),
        b"[fuzzer]\ntype=user\npassword=fuzz-password\n",
        b"[phone]\nexten => 100,1,Answer()\nsame => n,Stasis(fuzzer)\n",
        b"[global]\ncontrol_message_format=json\n",
        b"[broken\nname=value",
        b"\xff\0",
    ] {
        assert!(settings(source).is_ok());
    }
    assert!(matches!(
        settings(b"[general]\nbindaddr=127.0.0.1\n"),
        Ok(count) if count > 0
    ));
}

#[test]
fn media_controls_roundtrip_successful_plain_text_and_json_inputs() {
    for source in [
        b"ANSWER".as_slice(),
        b"MARK_MEDIA correlation",
        br#"{"command":"SET_MEDIA_DIRECTION","direction":"both"}"#,
        b"MEDIA_XOFF",
        br#"{"event":"MEDIA_XON","channel_id":"fuzzer"}"#,
        b"DTMF_END digit:# channel_id:fuzzer",
        b"STATUS channel_id:fuzzer queue_length:0 xon_level:1 xoff_level:2 queue_full:false bulk_media:false media_paused:false",
        br#"{"command":"UNKNOWN"}"#,
        b"\xff\0",
    ] {
        assert!(controls(source).is_ok());
    }
    assert!(matches!(controls(b"ANSWER"), Ok(count) if count > 0));
    assert!(matches!(
        controls(b"MEDIA_XOFF"),
        Ok(count) if count > 0
    ));
}

#[cfg(feature = "signaling")]
#[test]
fn sip_streams_preserve_messages_and_terminal_errors_across_fragmentation() {
    use rusterisk_input_fuzz::sip;
    let message = b"OPTIONS sip:fuzzer@localhost SIP/2.0\r\nContent-Length: 0\r\n\r\n";
    assert!(matches!(sip(message), Ok(count) if count > 0));
    let sdp = b"v=0\r\nc=IN IP4 127.0.0.1\r\nm=audio 9000 RTP/AVP 0 101\r\na=rtpmap:101 telephone-event/8000\r\na=fmtp:101 0-15\r\na=sendonly\r\n";
    assert!(matches!(sip(sdp), Ok(count) if count > 0));
    for bytes in [
        b"\r\n\r\n".as_slice(),
        b"OPTIONS sip:fuzzer SIP/2.0\r\nContent-Length: 2\r\n\r\n\0\xff\r\n",
        b"OPTIONS sip:fuzzer SIP/2.0\r\nContent-Length: 9\r\n\r\nshort",
        b"OPTIONS sip:fuzzer SIP/2.0\r\nContent-Length: 0\r\nl: 0\r\n\r\n",
        b"OPTIONS sip:fuzzer SIP/2.0\n",
    ] {
        assert!(sip(bytes).is_ok());
    }
    assert!(sip(&vec![b'A'; rusterisk_sip::MAX_MESSAGE + 1]).is_ok());
}

#[cfg(feature = "media")]
#[test]
fn media_packets_roundtrip_and_stream_fragmentation_preserves_audio_boundaries() {
    use rusterisk_input_fuzz::media;
    for bytes in [
        b"\0\0\0".as_slice(),
        b"\x03\0\x01#",
        b"\x10\0\x04\x01\x02\x03\x04",
        b"\x80\0\0\x01\0\0\0\x02\0\0\0\x03\x11\x22",
        b"\x05\x8a\0\xa0",
    ] {
        assert!(matches!(media(bytes), Ok(count) if count > 0));
    }
    for bytes in [
        b"".as_slice(),
        b"\x10\xff\xff",
        b"\x10\0\x03\x01\x02\x03",
        b"\xff\xff\xff\xff",
    ] {
        assert!(media(bytes).is_ok());
    }
    assert!(media(&vec![0; rusterisk_media::MAX_PACKET + 1]).is_ok());
}
