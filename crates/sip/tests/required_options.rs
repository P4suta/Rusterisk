use rsipstack::sip::{Header, Request, SipMessage};
use rusterisk_sip::MAX_MESSAGE;

#[path = "../src/required_options.rs"]
mod required_options;
use required_options::RequiredOptionsError;

fn packet(values: &[&str]) -> Result<Request, RequiredOptionsError> {
    let bytes = "INVITE sip:100@localhost SIP/2.0\r\nVia: SIP/2.0/UDP 127.0.0.1:5061;branch=z9hG4bK-required\r\nFrom: <sip:phone@localhost>;tag=caller\r\nTo: <sip:100@localhost>\r\nCall-ID: required-options\r\nCSeq: 1 INVITE\r\nContact: <sip:phone@127.0.0.1:5061>\r\nMax-Forwards: 70\r\nReplaces: ignored;to-tag=target;from-tag=source\r\nSupported: replaces\r\nProxy-Require: replaces\r\nContent-Length: 0\r\n\r\n";
    let SipMessage::Request(mut request) =
        SipMessage::try_from(bytes).map_err(|_| RequiredOptionsError::Malformed)?
    else {
        return Err(RequiredOptionsError::Malformed);
    };
    for value in values {
        request.headers.push(Header::Require((*value).into()));
    }
    Ok(request)
}

#[test]
fn unrelated_headers_do_not_require_an_extension() -> Result<(), RequiredOptionsError> {
    assert!(required_options::unsupported(&packet(&[])?)?.is_none());
    Ok(())
}

#[test]
fn every_required_field_and_comma_separated_tag_is_reported() -> Result<(), RequiredOptionsError> {
    for (values, expected) in [
        (["RePlAcEs", "100rel"], "Unsupported: replaces, 100rel\r\n"),
        (
            ["\t100REL , replaces \t", "timer"],
            "Unsupported: 100rel, replaces, timer\r\n",
        ),
        (
            ["replaces", "REPLACES"],
            "Unsupported: replaces, replaces\r\n",
        ),
    ] {
        let result = required_options::unsupported(&packet(&values)?)?
            .ok_or(RequiredOptionsError::Malformed)?;
        assert_eq!(result.response_headers(), expected);
    }
    Ok(())
}

#[test]
fn malformed_fields_remain_invalid_after_a_recognizable_tag() -> Result<(), RequiredOptionsError> {
    for value in [
        "",
        " ",
        "\t",
        ",replaces",
        "replaces,",
        "replaces,,timer",
        "replaces parameter",
        "replaces;parameter",
        "\"replaces\"",
        "replaces\r\n",
        "réplaces",
        "replaces/100rel",
    ] {
        assert!(
            required_options::unsupported(&packet(&["replaces", value])?).is_err(),
            "{value:?}"
        );
    }
    Ok(())
}

#[test]
fn option_tags_use_the_rfc_ascii_token_alphabet() -> Result<(), RequiredOptionsError> {
    for byte in u8::MIN..=u8::MAX {
        let value = format!("x{}x", char::from(byte));
        let inspected = required_options::unsupported(&packet(&[value.as_str()])?);
        if byte == b',' {
            assert_eq!(
                inspected?
                    .ok_or(RequiredOptionsError::Malformed)?
                    .response_headers(),
                "Unsupported: x, x\r\n"
            );
            continue;
        }
        let valid = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz-.!%*_+`'~"
            .contains(&byte);
        assert_eq!(inspected.is_ok(), valid, "{byte}");
    }
    Ok(())
}

#[test]
fn required_options_preserve_existing_packet_and_header_limits() -> Result<(), RequiredOptionsError>
{
    let prefix = "Unsupported: ";
    let longest = "x".repeat(MAX_MESSAGE - prefix.len() - "\r\n".len());
    let required = required_options::unsupported(&packet(&[longest.as_str()])?)?
        .ok_or(RequiredOptionsError::Malformed)?;
    assert_eq!(required.response_headers().len(), MAX_MESSAGE);
    assert!(required_options::unsupported(&packet(&[format!("{longest}x").as_str()])?).is_err());
    assert!(
        required_options::unsupported(&packet(&["x".repeat(MAX_MESSAGE + 1).as_str()])?).is_err()
    );
    let mut request = packet(&["replaces"])?;
    while request.headers.len() < 64 {
        request.headers.push(Header::Require("x".into()));
    }
    assert!(required_options::unsupported(&request)?.is_some());
    request.headers.push(Header::Require("x".into()));
    assert!(required_options::unsupported(&request).is_err());
    Ok(())
}
