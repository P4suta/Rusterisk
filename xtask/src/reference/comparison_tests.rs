use super::{ReferenceCase, ReferenceImage};
use crate::reference::{ImageIdentity, Interop, ReferenceError, SipTransferCase};
use std::io;

const ARI: &str = "resource_queries_and_app_subscriptions_match_the_reference";
const TAG: &str = "rusterisk-asterisk-reference:22.9.0";

fn passed(name: &str) -> String {
    format!("running 1 test\ntest {name} ... ok\n\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 5 filtered out; finished in 1.00s\n")
}

fn image(digit: char) -> Result<ImageIdentity, ReferenceError> {
    ImageIdentity::parse(&format!("sha256:{}", digit.to_string().repeat(64)))
        .ok_or(ReferenceError::InvalidIdentity)
}

#[test]
fn the_ari_case_selects_the_existing_ignored_test_and_an_exact_native_invocation() -> io::Result<()>
{
    let case = ReferenceCase::Ari;
    assert_eq!(case.name(), ARI);
    assert_eq!(
        case.cargo_arguments(),
        [
            "test",
            "--locked",
            "--package",
            "rusterisk-client",
            "--test",
            "reference",
            ARI,
            "--",
            "--ignored",
            "--exact",
            "--show-output",
        ]
    );
    let source = syn::parse_file(include_str!("../../../crates/client/tests/reference.rs"))
        .map_err(io::Error::other)?;
    let declarations = source
        .items
        .iter()
        .filter_map(|item| {
            if let syn::Item::Fn(function) = item {
                Some(function)
            } else {
                None
            }
        })
        .filter(|function| function.sig.ident == ARI)
        .collect::<Vec<_>>();
    let [function] = declarations.as_slice() else {
        return Err(io::Error::other("The required ARI test is not unique"));
    };
    assert!(function.sig.asyncness.is_some());
    assert!(function
        .attrs
        .iter()
        .any(|attribute| attribute.path().is_ident("ignore")));
    case.validate_execution(&passed(ARI))
        .map_err(io::Error::other)
}

#[test]
fn zero_ignored_other_missing_and_excess_ari_cases_cannot_complete_the_inventory() {
    let valid = passed(ARI);
    let cases = [
        valid.replace("1 passed", "0 passed"),
        valid
            .replace("1 passed", "0 passed")
            .replace("0 ignored", "1 ignored")
            .replace(" ... ok", " ... ignored"),
        passed("a_different_reference_test"),
        "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 5 filtered out; finished in 1.00s\n".into(),
        format!("test {ARI} ... ok\n"),
        valid
            .replace("1 passed", "2 passed")
            .replace("running 1 test", "running 2 tests"),
        format!("test {ARI} ... ok\n{valid}"),
        String::new(),
        "x".repeat(65_537),
    ];
    for output in cases {
        assert!(ReferenceCase::Ari.validate_execution(&output).is_err());
    }
}

#[test]
fn reference_transfer_cases_reuse_the_existing_case_identity_and_native_validator() -> io::Result<()>
{
    for transfer in SipTransferCase::ALL {
        let case = ReferenceCase::Transfer(transfer);
        assert_eq!(case.name(), transfer.name());
        let output = passed(transfer.name());
        transfer
            .validate_execution(&output)
            .map_err(io::Error::other)?;
        case.validate_execution(&output).map_err(io::Error::other)?;
        assert!(ReferenceCase::Ari.validate_execution(&output).is_err());
        assert!(case.validate_execution(&passed(ARI)).is_err());
    }
    Ok(())
}

#[test]
fn sipp_and_ari_transfer_start_from_the_same_selected_immutable_image() -> io::Result<()> {
    let selected = image('a').map_err(io::Error::other)?;
    let expected = selected.as_str().to_owned();
    let image = ReferenceImage::new(selected);
    let plan = Interop::new("shared-image", "/tmp/rusterisk-reference-fixture")
        .map_err(io::Error::other)?;
    let mut expected_arguments = plan.start().arguments;
    assert_eq!(
        expected_arguments
            .iter()
            .filter(|argument| *argument == TAG)
            .count(),
        1
    );
    for argument in &mut expected_arguments {
        if argument == TAG {
            *argument = expected.clone();
        }
    }
    let sipp = image.start(&plan).map_err(io::Error::other)?;
    let ari_transfer = image.start(&plan).map_err(io::Error::other)?;
    assert_eq!(sipp.arguments, expected_arguments);
    assert_eq!(ari_transfer.arguments, expected_arguments);
    image.check_start(&sipp).map_err(io::Error::other)?;
    image.check_start(&ari_transfer).map_err(io::Error::other)
}

#[test]
fn another_immutable_start_request_cannot_satisfy_the_selected_reference_image() -> io::Result<()> {
    assert!(ImageIdentity::parse(TAG).is_none());
    let selected = ReferenceImage::new(image('a').map_err(io::Error::other)?);
    let other = ReferenceImage::new(image('b').map_err(io::Error::other)?);
    let plan = Interop::new("image-mismatch", "/tmp/rusterisk-reference-fixture")
        .map_err(io::Error::other)?;
    let expected = selected.start(&plan).map_err(io::Error::other)?;
    let wrong = other.start(&plan).map_err(io::Error::other)?;
    selected.check_start(&expected).map_err(io::Error::other)?;
    assert!(selected.check_start(&wrong).is_err());
    assert!(selected.check_start(&plan.start()).is_err());
    Ok(())
}
