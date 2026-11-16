use super::{
    responses::{final_response, response},
    transaction, transmitted,
};
use rusterisk_sip::SipError;

fn seconds(protocol: &str, headers: &str) -> Result<Option<u32>, SipError> {
    let mut pending = transaction(protocol, 0)?;
    let packet = transmitted(pending.poll(0)?)?;
    let receipt =
        final_response(pending.receive(&response(&packet, 503, headers)?, packet.peer, 100)?)?;
    receipt
        .retry_after()
        .map(|seconds| seconds.map(|seconds| seconds.get()))
}

#[test]
fn positive_retry_instructions_accept_the_complete_header_grammar() -> Result<(), SipError> {
    for protocol in ["udp", "tcp", "tls"] {
        for (value, expected) in [
            ("1", 1),
            ("0007", 7),
            ("4294967295", u32::MAX),
            ("\t7 \t", 7),
            ("\r\n\t7", 7),
            ("7(maintenance)", 7),
            ("7 (mémo)", 7),
            ("7 ()", 7),
            (r"7 (outside (inside \) still) end)", 7),
            ("7 (escaped\\\r\n space)", 7),
            ("7; duration=9; flag; node=provider.example.", 7),
            ("7;duration=0;reason=\"value;with=punctuation\"", 7),
            ("7;reason=\"escaped\\\"value\"", 7),
            ("7;reason=\"escaped\\\x01\"", 7),
            ("7;reason=\"mémo\"", 7),
            ("7;reason=\"\";token=-.!%*_+`'~", 7),
            ("7;node=[2001:db8::1]", 7),
            ("7;node=[::ffff:192.0.2.1]", 7),
            ("7 (comment)\r\n ;duration=4294967295;flag", 7),
        ] {
            assert_eq!(
                seconds(protocol, &format!("Retry-After: {value}\r\n"))?,
                Some(expected)
            );
        }
    }
    Ok(())
}

#[test]
fn absent_or_zero_values_supply_no_positive_retry_instruction() -> Result<(), SipError> {
    for headers in [
        "",
        "Retry-After: 0\r\n",
        "Retry-After: 0000 (none);duration=0;flag\r\n",
    ] {
        assert_eq!(seconds("udp", headers)?, None);
    }
    Ok(())
}

#[test]
fn malformed_values_cannot_become_retry_deadlines() -> Result<(), SipError> {
    for value in [
        "",
        "+7",
        "-1",
        "4294967296",
        "7,8",
        "7 garbage",
        "7;",
        "7;;flag",
        "7;=value",
        "7;duration",
        "7;duration=+3",
        "7;duration=-1",
        "7;duration=4294967296",
        "7;duration=\"3\"",
        "7 (unterminated",
        "7 (extra))",
        "7 (first)(second)",
        "7;reason=\"unterminated",
        "7;reason=\"escaped\\\"",
        "7;reason=\"\\é\"",
        "7;reason=\"control\x01\"",
        "7;reason=some value",
        "7;reason=value (comment)",
        "7;node=[]",
        "7;node=[127.0.0.1]",
        "7;node=[2001:db8::1]:5060",
        "7;node=2001:db8::1",
        "0 (unterminated",
        "0;reason=\"unterminated",
    ] {
        assert!(
            seconds("udp", &format!("Retry-After: {value}\r\n")).is_err(),
            "{value:?}"
        );
    }
    Ok(())
}

#[test]
fn duplicate_headers_and_parameter_names_are_rejected() -> Result<(), SipError> {
    for headers in [
        "Retry-After: 7\r\nRetry-After: 7\r\n",
        "Retry-After: 7\r\nretry-after: 8\r\n",
        "Retry-After: 7;duration=1;DURATION=2\r\n",
        "Retry-After: 7;Foo=one;foo=two\r\n",
        "Retry-After: 7;flag;FLAG\r\n",
    ] {
        assert!(seconds("udp", headers).is_err());
    }
    Ok(())
}

#[test]
fn parameter_capacity_and_message_capacity_are_checked() -> Result<(), SipError> {
    let mut value = "7".to_owned();
    for index in 0..16 {
        value.push_str(&format!(";field-{index}=value"));
    }
    assert_eq!(
        seconds("udp", &format!("Retry-After: {value}\r\n"))?,
        Some(7)
    );
    value.push_str(";overflow=value");
    assert!(matches!(
        seconds("udp", &format!("Retry-After: {value}\r\n")),
        Err(SipError::Capacity)
    ));
    let oversized = format!("Retry-After: 7 ({})\r\n", "x".repeat(16_384));
    assert!(matches!(
        seconds("udp", &oversized),
        Err(SipError::Capacity)
    ));
    Ok(())
}

#[test]
fn nested_comments_require_balanced_delimiters_within_the_wire_limit() -> Result<(), SipError> {
    let nested = format!(
        "Retry-After: 7 {}text{}\r\n",
        "(".repeat(3000),
        ")".repeat(3000)
    );
    assert_eq!(seconds("udp", &nested)?, Some(7));
    let incomplete = nested.replace("text", "text)");
    assert!(seconds("udp", &incomplete).is_err());
    Ok(())
}
