use rsipstack::sip::{Method, Request, SipMessage, Uri};
use rusterisk_sip::{AttendedRefer, BlindRefer, ReferError, ReferSubscription, MAX_MESSAGE};

fn request(headers: &str) -> Result<Request, ReferError> {
    let source = format!("REFER sip:service@server.invalid SIP/2.0\r\nVia: SIP/2.0/UDP phone.invalid;branch=z9hG4bK-attended\r\nFrom: <sip:alice@phone.invalid>;tag=remote\r\nTo: <sip:service@server.invalid>;tag=local\r\nCall-ID: attended-request\r\nCSeq: 3 REFER\r\n{headers}Content-Length: 0\r\n\r\n");
    match SipMessage::try_from(source.as_str()).map_err(|_| ReferError::Invalid)? {
        SipMessage::Request(request) => Ok(request),
        SipMessage::Response(_) => Err(ReferError::Invalid),
    }
}

fn parse(target: &str) -> Result<AttendedRefer, ReferError> {
    AttendedRefer::parse(&request(&format!("Refer-To: <{target}>\r\n"))?)
}

#[test]
fn attended_input_retains_the_checked_uri_and_decoded_dialog_selector() -> Result<(), ReferError> {
    for target in [
        "sip:phone@target.invalid?Replaces=Case%40host%3Bto-tag%3DLocal%3Bfrom-tag%3DRemote",
        "sips:phone@[::1]:5061;transport=tcp?%52ePlAcEs=Case%40host%3bto-tag%3dLocal%3bfrom-tag%3dRemote",
    ] {
        let target_uri = Uri::try_from(target).map_err(|_| ReferError::Invalid)?;
        let parsed = parse(target)?;
        assert_eq!(parsed.uri(), &target_uri);
        assert_eq!(parsed.replaces().call_id(), "Case@host");
        assert_eq!(parsed.replaces().to_tag(), "Local");
        assert_eq!(parsed.replaces().from_tag(), "Remote");
        assert!(!parsed.replaces().early_only());
        assert_eq!(parsed.subscription(), ReferSubscription::Notify);
        assert!(matches!(
            BlindRefer::parse(&request(&format!("Refer-To: <{target}>\r\n"))?),
            Err(ReferError::Unsupported),
        ));
    }
    Ok(())
}

#[test]
fn selector_escapes_decode_once_and_subscription_suppression_stays_independent(
) -> Result<(), ReferError> {
    let request = request("r: <sip:phone@target.invalid?Replaces=dialog%253Bto-tag%253Dfake%3Bto-tag%3DLocal%3Bfrom-tag%3DRemote%3Bearly-only>\r\nRefer-Sub: false\r\n")?;
    let parsed = AttendedRefer::parse(&request)?;
    assert_eq!(parsed.replaces().call_id(), "dialog%3Bto-tag%3Dfake");
    assert_eq!(parsed.replaces().to_tag(), "Local");
    assert_eq!(parsed.replaces().from_tag(), "Remote");
    assert!(parsed.replaces().early_only());
    assert_eq!(parsed.subscription(), ReferSubscription::Suppressed);
    Ok(())
}

#[test]
fn attended_input_requires_one_valid_replaces_value_without_blind_fallback() {
    for target in [
        "sip:phone@target.invalid",
        "sip:phone@target.invalid?Replaces=",
        "sip:phone@target.invalid?Replaces=dialog",
        "sip:phone@target.invalid?Replaces=dialog%3Bto-tag%3Dlocal",
        "sip:phone@target.invalid?Replaces=dialog%3Bto-tag%3Dlocal%3Bfrom-tag%3Dremote%3Bto-tag%3Dother",
        "sip:phone@target.invalid?Replaces=dialog%3Bto-tag%3Dlocal%3Bfrom-tag%3Dremote%0d%0aInjected%3Avalue",
        "sip:phone@target.invalid?Replaces=dialog%3Bto-tag%3Dlocal%3Bfrom-tag%3Dremote&Replaces=other%3Bto-tag%3Dx%3Bfrom-tag%3Dy",
    ] {
        assert_eq!(parse(target), Err(ReferError::Invalid), "{target}");
    }
}

#[test]
fn unsupported_methods_schemes_and_parameter_replaces_remain_explicit() -> Result<(), ReferError> {
    for target in [
        "sip:phone@target.invalid;Replaces=dialog%3Bto-tag%3Dlocal%3Bfrom-tag%3Dremote",
        "sip:phone@target.invalid;%52eplaces=dialog%3Bto-tag%3Dlocal%3Bfrom-tag%3Dremote",
        "sip:phone@target.invalid;Replaces=dialog%3Bto-tag%3Dlocal%3Bfrom-tag%3Dremote?Replaces=dialog%3Bto-tag%3Dlocal%3Bfrom-tag%3Dremote",
        "sip:phone@target.invalid;Replaces=dialog%3Bto-tag%3Dlocal%3Bfrom-tag%3Dremote?%52eplaces=other%3Bto-tag%3Dx%3Bfrom-tag%3Dy",
        "sip:phone@target.invalid;method=BYE?Replaces=dialog%3Bto-tag%3Dlocal%3Bfrom-tag%3Dremote",
        "sip:phone@target.invalid?Replaces=dialog%3Bto-tag%3Dlocal%3Bfrom-tag%3Dremote&method=SUBSCRIBE",
        "https://target.invalid/call?Replaces=dialog%3Bto-tag%3Dlocal%3Bfrom-tag%3Dremote",
    ] {
        assert_eq!(parse(target), Err(ReferError::Unsupported), "{target}");
    }
    assert_eq!(
        AttendedRefer::parse(&request(
            "Refer-To: <sip:phone@target.invalid>;Replaces=dialog\r\n"
        )?),
        Err(ReferError::Unsupported),
    );
    Ok(())
}

#[test]
fn attended_uri_component_limits_apply_to_query_headers_and_uri_parameters(
) -> Result<(), ReferError> {
    let selector = "Replaces=dialog%3Bto-tag%3Dlocal%3Bfrom-tag%3Dremote";
    for (extra, accepted) in [(15, true), (16, false)] {
        let headers = (0..extra)
            .map(|index| format!("&X-{index}=value"))
            .collect::<String>();
        let parsed = parse(&format!("sip:phone@target.invalid?{selector}{headers}"));
        if accepted {
            assert_eq!(parsed?.replaces().call_id(), "dialog");
        } else {
            assert_eq!(parsed, Err(ReferError::Capacity));
        }
    }
    for (extra, accepted) in [(32, true), (33, false)] {
        let parameters = (0..extra)
            .map(|index| format!(";x-{index}=value"))
            .collect::<String>();
        let parsed = parse(&format!("sip:phone@target.invalid{parameters}?{selector}"));
        if accepted {
            assert_eq!(parsed?.replaces().call_id(), "dialog");
        } else {
            assert_eq!(parsed, Err(ReferError::Capacity));
        }
    }
    Ok(())
}

#[test]
fn complete_request_bounds_and_subscription_validation_precede_attended_admission(
) -> Result<(), ReferError> {
    let target = "Refer-To: <sip:phone@target.invalid?Replaces=dialog%3Bto-tag%3Dlocal%3Bfrom-tag%3Dremote>\r\n";
    let mut oversized = request(target)?;
    oversized.body = [0; MAX_MESSAGE].into_iter().collect();
    assert_eq!(AttendedRefer::parse(&oversized), Err(ReferError::Capacity));
    let mut invitation = request(target)?;
    invitation.method = Method::Invite;
    assert_eq!(AttendedRefer::parse(&invitation), Err(ReferError::Invalid));
    for other in [
        "Refer-Sub: maybe\r\n",
        "Refer-Sub: false\r\nRefer-Sub: true\r\n",
        target,
    ] {
        assert_eq!(
            AttendedRefer::parse(&request(&format!("{target}{other}"))?),
            Err(ReferError::Invalid),
        );
    }
    Ok(())
}

#[test]
fn attended_debug_omits_uri_credentials_and_dialog_identifiers() -> Result<(), ReferError> {
    let parsed = parse("sip:private-user:private-password@target.invalid?Replaces=private-dialog%3Bto-tag%3Dprivate-local%3Bfrom-tag%3Dprivate-remote")?;
    let output = format!("{parsed:?}");
    for value in [
        "private-user",
        "private-password",
        "private-dialog",
        "private-local",
        "private-remote",
    ] {
        assert!(!output.contains(value));
    }
    Ok(())
}
