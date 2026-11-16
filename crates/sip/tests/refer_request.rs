use rsipstack::sip::{headers::untyped::ReferTo, Header, Request, SipMessage};
use rusterisk_sip::{BlindRefer, ReferError, ReferSubscription, MAX_MESSAGE};

fn request(headers: &str) -> Result<Request, ReferError> {
    let packet = format!(
        "REFER sip:service@server.invalid SIP/2.0\r\nVia: SIP/2.0/UDP phone.invalid;branch=z9hG4bK-refer\r\nFrom: <sip:alice@phone.invalid>;tag=remote\r\nTo: <sip:service@server.invalid>;tag=local\r\nCall-ID: refer-request\r\nCSeq: 3 REFER\r\n{headers}Content-Length: 0\r\n\r\n"
    );
    match SipMessage::try_from(packet.as_str()).map_err(|_| ReferError::Invalid)? {
        SipMessage::Request(request) => Ok(request),
        SipMessage::Response(_) => Err(ReferError::Invalid),
    }
}

fn parse(headers: &str) -> Result<BlindRefer, ReferError> {
    BlindRefer::parse(&request(headers)?)
}

#[test]
fn sip_targets_keep_the_decoded_user_as_a_local_extension() -> Result<(), ReferError> {
    for (source, extension) in [
        ("sip:200@target.invalid", "200"),
        ("<sip:%32%30%30@target.invalid>", "200"),
        (
            r#""Support, desk" <sips:Room%2f42@[::1]:5061;transport=tcp?Subject=Help>;extension=kept"#,
            "Room/42",
        ),
        ("<sip:+1202;ext=7@target.invalid;user=phone>", "+1202;ext=7"),
        ("sip:target.invalid", "s"),
        ("<SIP:200@target.invalid;method=INVITE>", "200"),
    ] {
        let parsed = parse(&format!("Refer-To: {source}\r\n"))?;
        assert_eq!(parsed.extension(), extension, "{source}");
        assert_eq!(parsed.subscription(), ReferSubscription::Notify);
    }
    assert_eq!(parse("r: <sip:200@target.invalid>\r\n")?.extension(), "200");
    Ok(())
}

#[test]
fn exactly_one_target_is_required_even_with_compact_headers_or_address_lists() {
    for headers in [
        "",
        "Refer-To: <sip:200@target.invalid>\r\nRefer-To: <sip:300@target.invalid>\r\n",
        "Refer-To: <sip:200@target.invalid>\r\nr: <sip:300@target.invalid>\r\n",
        "Refer-To: <sip:200@target.invalid>, <sip:300@target.invalid>\r\n",
        "Refer-To: sip:200@target.invalid, sip:300@target.invalid\r\n",
        "Refer-To: \r\n",
    ] {
        assert!(matches!(parse(headers), Err(ReferError::Invalid)));
    }
}

#[test]
fn malformed_framing_uri_components_and_decoded_controls_are_rejected() {
    for source in [
        "<<sip:200@target.invalid>>",
        "<sip:200@target.invalid",
        "sip:200@target.invalid>",
        "<sip:200@target.invalid>junk",
        "<sip:200@target.invalid>;;",
        "<sip:200@target.invalid>;extension=\"unclosed",
        "<sip:200@target.invalid>;extension=\"value\"junk",
        "\"unclosed <sip:200@target.invalid>",
        "<sip:200@>",
        "<sip:@target.invalid>",
        "<sip:200@@target.invalid>",
        "<sip:200@[::1]junk>",
        "<sip:200@target.invalid:70000>",
        "<sip:200@target^invalid>",
        "<sip:200@target.invalid;>",
        "<sip:200@target.invalid;transport=udp;Transport=tcp>",
        "<sip:200@target.invalid?Subject>",
        "<sip:200@target.invalid?=value>",
        "<sip:200@target.invalid?Subject=value&>",
        "<sip:%@target.invalid>",
        "<sip:%2@target.invalid>",
        "<sip:%GG@target.invalid>",
        "<sip:200%00@target.invalid>",
        "<sip:200%0a@target.invalid>",
        "<sip:200%0D@target.invalid>",
        "<sip:200@target.invalid?Subject=%0d%0aHeader%3Avalue>",
    ] {
        assert!(
            matches!(
                parse(&format!("Refer-To: {source}\r\n")),
                Err(ReferError::Invalid)
            ),
            "{source}"
        );
    }
}

#[test]
fn replaces_and_non_invite_resources_cannot_become_blind_transfers() {
    for source in [
        "<sip:200@target.invalid?Replaces=dialog%3Bto-tag%3Dlocal%3Bfrom-tag%3Dremote>",
        "<sip:200@target.invalid?%52ePlAcEs=dialog>",
        "<sip:200@target.invalid;Replaces=dialog>",
        "<sip:200@target.invalid;%52eplaces=dialog>",
        "<sip:200@target.invalid>;Replaces=dialog",
        "<sip:200@target.invalid;method=SUBSCRIBE>",
        "<sip:200@target.invalid?Method=BYE>",
        "<tel:+12025550100>",
        "<https://target.invalid/call>",
    ] {
        assert!(
            matches!(
                parse(&format!("Refer-To: {source}\r\n")),
                Err(ReferError::Unsupported)
            ),
            "{source}"
        );
    }
}

#[test]
fn refer_subscription_suppression_has_a_checked_independent_value() -> Result<(), ReferError> {
    for (header, expected) in [
        ("", ReferSubscription::Notify),
        ("Refer-Sub: true\r\n", ReferSubscription::Notify),
        ("Refer-Sub: false\r\n", ReferSubscription::Suppressed),
        (
            "Refer-Sub: FALSE;extension=\"semi;colon\"\r\n",
            ReferSubscription::Suppressed,
        ),
    ] {
        let parsed = parse(&format!("Refer-To: <sip:200@target.invalid>\r\n{header}"))?;
        assert_eq!(parsed.subscription(), expected);
    }
    for header in [
        "Refer-Sub: false\r\nRefer-Sub: true\r\n",
        "Refer-Sub: maybe\r\n",
        "Refer-Sub: \"false\"\r\n",
        "Refer-Sub: false,true\r\n",
        "Refer-Sub: false;\r\n",
        "Refer-Sub: false;extension=\"unterminated\r\n",
        "Refer-Sub: false;=value\r\n",
    ] {
        assert!(
            matches!(
                parse(&format!("Refer-To: <sip:200@target.invalid>\r\n{header}")),
                Err(ReferError::Invalid)
            ),
            "{header}"
        );
    }
    Ok(())
}

#[test]
fn refer_sub_whitespace_cannot_turn_injected_headers_into_values() -> Result<(), ReferError> {
    for value in [
        "false\r\n",
        "\ntrue",
        "\u{2000}false",
        "true;\u{2000}extension=value",
    ] {
        let mut request = request("Refer-To: <sip:200@target.invalid>\r\n")?;
        request
            .headers
            .push(Header::Other("Refer-Sub".into(), value.into()));
        assert!(
            matches!(BlindRefer::parse(&request), Err(ReferError::Invalid)),
            "{value:?}"
        );
    }
    Ok(())
}

#[test]
fn uri_and_extension_parameter_counts_have_explicit_capacity_limits() -> Result<(), ReferError> {
    for (count, accepted) in [(32, true), (33, false)] {
        let parameters = (0..count)
            .map(|index| format!(";x-{index}=value"))
            .collect::<String>();
        for headers in [
            format!("Refer-To: <sip:200@target.invalid{parameters}>\r\n"),
            format!("Refer-To: <sip:200@target.invalid>{parameters}\r\n"),
            format!("Refer-To: <sip:200@target.invalid>\r\nRefer-Sub: false{parameters}\r\n"),
        ] {
            let parsed = parse(&headers);
            if accepted {
                assert_eq!(parsed?.extension(), "200");
            } else {
                assert!(matches!(parsed, Err(ReferError::Capacity)));
            }
        }
    }
    for (count, accepted) in [(16, true), (17, false)] {
        let headers = (0..count)
            .map(|index| format!("X-Value-{index}=value"))
            .collect::<Vec<_>>()
            .join("&");
        let parsed = parse(&format!("Refer-To: <sip:200@target.invalid?{headers}>\r\n"));
        if accepted {
            assert_eq!(parsed?.extension(), "200");
        } else {
            assert!(matches!(parsed, Err(ReferError::Capacity)));
        }
    }
    Ok(())
}

#[test]
fn the_checked_boundary_limits_input_and_keeps_uri_credentials_out_of_debug(
) -> Result<(), ReferError> {
    let mut injected = request("Refer-To: <sip:200@target.invalid>\r\n")?;
    injected.headers.unique_push(Header::ReferTo(ReferTo::new(
        "<sip:200@target.invalid>\r\nInjected: value",
    )));
    assert!(matches!(
        BlindRefer::parse(&injected),
        Err(ReferError::Invalid)
    ));
    let oversized = format!(
        "Refer-To: <sip:{}@target.invalid>\r\n",
        "2".repeat(MAX_MESSAGE)
    );
    assert!(matches!(parse(&oversized), Err(ReferError::Capacity)));
    let mut many = request("Refer-To: <sip:200@target.invalid>\r\n")?;
    for index in 0..64 {
        many.headers
            .push(Header::Other(format!("X-Value-{index}"), String::new()));
    }
    assert!(matches!(
        BlindRefer::parse(&many),
        Err(ReferError::Capacity)
    ));
    let mut body = request("Refer-To: <sip:200@target.invalid>\r\n")?;
    body.body = [0].into_iter().cycle().take(MAX_MESSAGE + 1).collect();
    assert!(matches!(
        BlindRefer::parse(&body),
        Err(ReferError::Capacity)
    ));
    let parsed =
        parse("Refer-To: \"Private display\" <sip:200:private-password@target.invalid>\r\n")?;
    assert_eq!(parsed.extension(), "200");
    let debug = format!("{parsed:?}");
    assert!(!debug.contains("Private display"));
    assert!(!debug.contains("private-password"));
    assert!(!debug.contains("target.invalid"));
    let mut invite = request("Refer-To: <sip:200@target.invalid>\r\n")?;
    invite.method = rsipstack::sip::Method::Invite;
    assert!(matches!(
        BlindRefer::parse(&invite),
        Err(ReferError::Invalid)
    ));
    Ok(())
}
