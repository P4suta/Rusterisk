use super::{Credentials, DigestError, DigestRequest, Header};

fn response(username: &str, password: &str, encoding: Option<&str>) -> Result<String, DigestError> {
    let encoding = encoding
        .map(|encoding| format!(", charset={encoding}"))
        .unwrap_or_default();
    let challenge = Header::parse(&format!(
        "Digest realm=\"example.net\", nonce=\"server-nonce\", algorithm=SHA-256, qop=\"auth-int\"{encoding}"
    ))?
    .challenge()?;
    let credentials = Credentials::new(username, password, challenge.realm())?;
    let request = DigestRequest::new(
        "REGISTER",
        "sip:registrar.example.net",
        challenge.nonce(),
        b"v=0\r\ns=call\r\n",
    )?;
    challenge.authorization(&credentials, &request, "client-nonce", 1)
}

#[test]
fn negotiated_utf8_keeps_the_independent_ascii_integrity_vector() -> Result<(), DigestError> {
    for encoding in ["UTF-8", "utf-8", "\"UTF-8\""] {
        let wire = response("alice", "sample-password", Some(encoding))?;
        assert!(wire.contains(
            "response=\"877579be0585f470537b474714e1b081f147be3e64929fc7fd7feef79fa16404\""
        ));
        assert!(wire.contains("username=\"alice\""));
        assert!(!wire.contains("charset="));
    }
    Ok(())
}

#[test]
fn negotiated_utf8_prepares_credentials_and_preserves_case() -> Result<(), DigestError> {
    let prepared = response("Al\u{00e9} Smith", "p\u{00e5}ss word", Some("UTF-8"))?;
    assert!(prepared
        .contains("response=\"27e5b45870b87bef776f15ea299541615e07fd987b5f136889ca0a4b0b1a0e3a\""));
    assert_eq!(
        response(
            "\u{ff21}le\u{0301} Smith",
            "pa\u{030a}ss\u{00a0}word",
            Some("UTF-8")
        )?,
        prepared
    );
    assert!(prepared.contains("username*=UTF-8''Al%C3%A9%20Smith"));
    assert!(!prepared.contains("username="));
    assert_ne!(
        response("al\u{00e9} Smith", "p\u{00e5}ss word", Some("UTF-8"))?,
        prepared
    );
    Ok(())
}

#[test]
fn absent_encoding_keeps_the_supplied_credential_octets() -> Result<(), DigestError> {
    assert_ne!(
        response("Al\u{00e9}", "p\u{00e5}ss", None)?,
        response("Ale\u{0301}", "pa\u{030a}ss", None)?
    );
    assert_eq!(
        response("Al\u{00e9}", "p\u{00e5}ss", Some("UTF-8"))?,
        response("Ale\u{0301}", "pa\u{030a}ss", Some("UTF-8"))?
    );
    Ok(())
}

#[test]
fn utf8_profiles_reject_prohibited_codepoints_and_overflow_after_preparation() {
    for username in [
        "a\u{e000}",
        "a\u{200d}b",
        "a\u{0627}",
        "a\u{ff1a}b",
        "a\u{00a0}b",
        " leading",
        "trailing ",
    ] {
        assert!(response(username, "sample", Some("UTF-8")).is_err());
    }
    for password in ["", "a\u{e000}", "a\u{2060}b"] {
        assert!(response("account", password, Some("UTF-8")).is_err());
    }
    assert!(response(&"\u{0344}".repeat(128), "sample", Some("UTF-8")).is_err());
    assert!(response("account", &"\u{0344}".repeat(1025), Some("UTF-8")).is_err());
}
