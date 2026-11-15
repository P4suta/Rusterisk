use rsipstack::sip::{Param, Transport};
use rusterisk_sip::{
    parse_registration_contacts, ContactError, RegistrationContact, RegistrationContacts,
};

fn one(uri: &str) -> Result<RegistrationContact, ContactError> {
    let source = format!("<{uri}>");
    let RegistrationContacts::Bindings(contacts) =
        parse_registration_contacts([source.as_str()], None)?
    else {
        return Err(ContactError::Invalid);
    };
    contacts
        .as_slice()
        .first()
        .cloned()
        .ok_or(ContactError::Invalid)
}

#[test]
fn repeated_contact_lists_preserve_quoted_boundaries_uri_parameters_and_owned_values(
) -> Result<(), ContactError> {
    let first = r#""A, \"Quoted\"" <sip:first@one.invalid;transport=udp;extension=kept?Subject=call>;expires=30;q=0.250, sip:second@two.invalid;expires=0"#.to_owned();
    let parsed = parse_registration_contacts(
        [first.as_str(), "<sips:third@[::1];transport=tcp>;q=1.0"],
        Some(120),
    )?;
    drop(first);
    let RegistrationContacts::Bindings(contacts) = parsed else {
        return Err(ContactError::Invalid);
    };
    let contacts = contacts.as_slice();
    assert_eq!(contacts.len(), 3);
    assert_eq!(contacts[0].expires(), Some(30));
    assert_eq!(contacts[0].preference(), 250);
    assert!(contacts[0]
        .source_uri()
        .contains(";transport=udp;extension=kept"));
    assert!(contacts[0]
        .uri()
        .params
        .iter()
        .any(|parameter| matches!(parameter, Param::Transport(Transport::Udp))));
    assert_eq!(
        contacts[0].uri().headers,
        [("Subject".into(), "call".into())]
    );
    assert_eq!(contacts[1].uri().to_string(), "sip:second@two.invalid");
    assert_eq!(contacts[1].expires(), Some(0));
    assert_eq!(contacts[1].preference(), 1000);
    assert_eq!(contacts[2].expires(), Some(120));
    assert!(!format!("{contacts:?}").contains("first@"));
    Ok(())
}

#[test]
fn queries_and_wildcard_removal_are_distinct_validated_states() -> Result<(), ContactError> {
    assert!(matches!(
        parse_registration_contacts([], Some(300))?,
        RegistrationContacts::Query
    ));
    assert!(matches!(
        parse_registration_contacts(["*"], Some(0))?,
        RegistrationContacts::RemoveAll
    ));
    for (values, expires) in [
        (["*", "*"].as_slice(), Some(0)),
        (["*", "<sip:a@one.invalid>"].as_slice(), Some(0)),
        (["*"].as_slice(), None),
        (["*"].as_slice(), Some(1)),
        (["*;expires=0"].as_slice(), Some(0)),
    ] {
        assert!(parse_registration_contacts(values.iter().copied(), expires).is_err());
    }
    Ok(())
}

#[test]
fn expiration_and_preference_parameters_reject_ambiguous_or_non_decimal_values() {
    for source in [
        "<sip:a@one.invalid>;expires=1;Expires=2",
        "<sip:a@one.invalid>;expires",
        "<sip:a@one.invalid>;expires=+1",
        "<sip:a@one.invalid>;expires=4294967296",
        "<sip:a@one.invalid>;expires=\"1\"",
        "<sip:a@one.invalid>;q=0.5;Q=0.6",
        "<sip:a@one.invalid>;q",
        "<sip:a@one.invalid>;q=NaN",
        "<sip:a@one.invalid>;q=-0.1",
        "<sip:a@one.invalid>;q=.5",
        "<sip:a@one.invalid>;q=1.001",
        "<sip:a@one.invalid>;q=0.1234",
        "<sip:a@one.invalid>;q=\"0.5\"",
    ] {
        assert!(
            parse_registration_contacts([source], None).is_err(),
            "{source}"
        );
    }
}

#[test]
fn malformed_entry_boundaries_and_header_injection_are_rejected() {
    for source in [
        "",
        ",<sip:a@one.invalid>",
        "<sip:a@one.invalid>,",
        "<sip:a@one.invalid>,, <sip:b@two.invalid>",
        "\"Unclosed <sip:a@one.invalid>",
        "<sip:a@one.invalid",
        "<<sip:a@one.invalid>>",
        "sip:a@one.invalid>",
        "<sip:a@one.invalid>missing=value",
        "<sip:a@one.invalid>;;",
        "<sip:a@one.invalid>;extension=\"x\r\nHeader: value\"",
    ] {
        assert!(
            parse_registration_contacts([source], None).is_err(),
            "{source}"
        );
    }
}

#[test]
fn contact_entry_uri_and_source_limits_apply_before_unbounded_parsing() -> Result<(), ContactError>
{
    let mut source = (0..32)
        .map(|index| format!("<sip:device{index}@example.net>"))
        .collect::<Vec<_>>()
        .join(",");
    let RegistrationContacts::Bindings(contacts) =
        parse_registration_contacts([source.as_str()], None)?
    else {
        return Err(ContactError::Invalid);
    };
    assert_eq!(contacts.as_slice().len(), 32);
    source.push_str(",<sip:extra@example.net>");
    assert!(matches!(
        parse_registration_contacts([source.as_str()], None),
        Err(ContactError::Capacity)
    ));
    assert!(matches!(
        parse_registration_contacts(["x".repeat(16385).as_str()], None),
        Err(ContactError::Capacity)
    ));
    let uri = format!("<sip:device@{}>", "x".repeat(2048));
    assert!(matches!(
        parse_registration_contacts([uri.as_str()], None),
        Err(ContactError::Capacity)
    ));
    Ok(())
}

#[test]
fn uri_equivalence_preserves_component_roles_and_parameter_presence() -> Result<(), ContactError> {
    for (left, right) in [
        (
            "sip:%61lice@EXAMPLE.com;transport=UDP",
            "sip:alice@example.COM;Transport=udp",
        ),
        (
            "sip:alice@example.com;extension=one",
            "sip:alice@example.com",
        ),
        (
            "sip:alice@example.com;method=REGISTER;transport=tcp",
            "sip:alice@example.com;transport=TCP;method=REGISTER",
        ),
        (
            "sip:alice@example.com?subject=Call&priority=urgent",
            "sip:alice@example.com?Priority=Urgent&Subject=Call",
        ),
    ] {
        let left = one(left)?;
        let right = one(right)?;
        assert!(left.equivalent(&right));
        assert!(right.equivalent(&left));
    }
    for (left, right) in [
        ("sip:Alice@example.com", "sip:alice@example.com"),
        ("sip:alice@example.com", "sips:alice@example.com"),
        ("sip:alice@example.com", "sip:alice@example.com:5060"),
        (
            "sip:alice@example.com",
            "sip:alice@example.com;transport=udp",
        ),
        ("sip:alice@example.com", "sip:alice@example.com;user=phone"),
        (
            "sip:alice@example.com",
            "sip:alice@example.com;maddr=239.1.1.1",
        ),
        (
            "sip:alice@example.com",
            "sip:alice@example.com?Subject=Call",
        ),
        (
            "sip:alice@example.com;method=REGISTER",
            "sip:alice@example.com;method=register",
        ),
        (
            "sip:alice@example.com?Subject=Call&Subject=Call",
            "sip:alice@example.com?Subject=Call&Subject=Other",
        ),
        (
            "sip:alice/example@example.com",
            "sip:alice%2fexample@example.com",
        ),
    ] {
        let left = one(left)?;
        let right = one(right)?;
        assert!(!left.equivalent(&right));
        assert!(!right.equivalent(&left));
    }
    assert!(one("sip:alice%2Fexample@example.com")?
        .equivalent(&one("sip:alice%2fexample@example.com")?));
    let bare = one("sip:alice@example.com")?;
    let enabled = one("sip:alice@example.com;security=on")?;
    let disabled = one("sip:alice@example.com;security=off")?;
    assert!(bare.equivalent(&enabled));
    assert!(bare.equivalent(&disabled));
    assert!(!enabled.equivalent(&disabled));
    Ok(())
}

#[test]
fn invalid_escapes_and_duplicate_uri_parameters_cannot_create_bindings() {
    for source in [
        "sip:a%zz@example.com",
        "sip:a@%65xample.com",
        "sip:a@example.com;transport=udp;Transport=tcp",
        "sip:a@example.com;extension=one;%65xtension=two",
        "sip:a@example.com;extension=one;",
        "sip:a@example.com;;extension=one",
    ] {
        assert!(one(source).is_err(), "{source}");
    }
}
