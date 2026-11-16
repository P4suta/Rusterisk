use super::{CredentialEncoding, Credentials, DigestError, DigestRequest, Header};

fn received(identity: &str) -> String {
    format!("Digest {identity}, realm=\"example.net\", nonce=\"server-nonce\", uri=\"sip:registrar.example.net\", response=\"877579be0585f470537b474714e1b081f147be3e64929fc7fd7feef79fa16404\", algorithm=SHA-256, qop=auth-int, cnonce=\"client-nonce\", nc=00000001")
}

#[test]
fn username_hashing_changes_the_wire_identity_after_calculating_the_response(
) -> Result<(), DigestError> {
    for (algorithm, username, password, realm, expected) in [
        (
            "MD5",
            "alice",
            "sample-password",
            "example.net",
            "31e996bf965775b703d6866768fcb4d5",
        ),
        (
            "MD5-sess",
            "alice",
            "sample-password",
            "example.net",
            "31e996bf965775b703d6866768fcb4d5",
        ),
        (
            "SHA-256",
            "alice",
            "sample-password",
            "example.net",
            "a77cfb90c6a66e542faaea176bec215b486ea0b9ea8b6a6449ff11d1efd4c2cf",
        ),
        (
            "SHA-256-sess",
            "alice",
            "sample-password",
            "example.net",
            "a77cfb90c6a66e542faaea176bec215b486ea0b9ea8b6a6449ff11d1efd4c2cf",
        ),
        (
            "SHA-512-256",
            "Jäsøn Doe",
            "Secret, or not?",
            "api@example.org",
            "793263caabb707a56211940d90411ea4a575adeccb7e360aeb624ed06ece9b0b",
        ),
        (
            "SHA-512-256-sess",
            "Jäsøn Doe",
            "Secret, or not?",
            "api@example.org",
            "793263caabb707a56211940d90411ea4a575adeccb7e360aeb624ed06ece9b0b",
        ),
    ] {
        let source = format!("Digest realm=\"{realm}\", nonce=\"server-nonce\", algorithm={algorithm}, qop=\"auth-int\", charset=UTF-8");
        let plain = Header::parse(&source)?.challenge()?;
        let hashed = Header::parse(&format!("{source}, userhash=true"))?.challenge()?;
        let credentials = Credentials::new(username, password, realm)?;
        let request = DigestRequest::new(
            "REGISTER",
            "sip:registrar.example.net",
            "server-nonce",
            b"v=0\r\ns=call\r\n",
        )?;
        let plain =
            Header::parse(&plain.authorization(&credentials, &request, "client-nonce", 1)?)?;
        let hashed =
            Header::parse(&hashed.authorization(&credentials, &request, "client-nonce", 1)?)?;
        assert_eq!(hashed.get("username"), Some(expected));
        assert_eq!(hashed.get("userhash"), Some("true"));
        assert!(hashed.get("username*").is_none());
        assert_eq!(hashed.get("response"), plain.get("response"));
        if algorithm == "SHA-256" {
            assert_eq!(
                hashed.get("response"),
                Some("877579be0585f470537b474714e1b081f147be3e64929fc7fd7feef79fa16404")
            );
        }
    }
    Ok(())
}

#[test]
fn extended_username_decodes_once_before_digest_verification() -> Result<(), DigestError> {
    let source = "Digest username*=UTF-8'en'Al%C3%A9%20Smith, realm=\"example.net\", nonce=\"server-nonce\", uri=\"sip:registrar.example.net\", response=\"27e5b45870b87bef776f15ea299541615e07fd987b5f136889ca0a4b0b1a0e3a\", algorithm=SHA-256, qop=auth-int, cnonce=\"client-nonce\", nc=00000001, userhash=false";
    for source in [
        source.to_owned(),
        source.replace("UTF-8'en'Al%C3%A9", "ISO-8859-1''Al%E9"),
    ] {
        let header = Header::parse(&source)?;
        let authorization = header.authorization()?;
        assert_eq!(authorization.username(), "Alé Smith");
        assert!(authorization.verify("REGISTER", b"v=0\r\ns=call\r\n", "påss word")?);
        assert!(!authorization.verify("REGISTER", b"v=0\r\ns=changed\r\n", "påss word")?);
    }
    Ok(())
}

#[test]
fn hashed_identity_resolves_the_prepared_username_with_the_challenge_realm(
) -> Result<(), DigestError> {
    let header = Header::parse(&received("username=\"a77cfb90c6a66e542faaea176bec215b486ea0b9ea8b6a6449ff11d1efd4c2cf\", userhash=true"))?;
    let authorization = header.authorization()?;
    assert!(authorization.matches_username("alice", CredentialEncoding::Utf8)?);
    assert!(authorization.matches_username("\u{ff41}lice", CredentialEncoding::Utf8)?);
    assert!(!authorization.matches_username("\u{ff41}lice", CredentialEncoding::Unspecified)?);
    assert!(!authorization.matches_username("Alice", CredentialEncoding::Utf8)?);
    assert!(!authorization.matches_username("bob", CredentialEncoding::Utf8)?);
    assert!(authorization
        .verify("REGISTER", b"", "sample-password")
        .is_err());
    Ok(())
}

#[test]
fn extended_username_accepts_attribute_characters_and_preserves_escaped_percent(
) -> Result<(), DigestError> {
    for (source, expected) in [
        ("UTF-8''alice!#$&+-.^_`|~", "alice!#$&+-.^_`|~"),
        ("uTf-8'en-US'alice%2520", "alice%20"),
        ("UTF-8''Al%c3%a9", "Alé"),
        ("ISO-8859-1''Al%e9", "Alé"),
    ] {
        let header = Header::parse(&received(&format!("username*={source}")))?;
        assert_eq!(header.authorization()?.username(), expected);
    }
    let name = "a".repeat(256);
    let header = Header::parse(&received(&format!("username*=UTF-8''{name}")))?;
    assert_eq!(header.authorization()?.username(), name);
    Ok(())
}

#[test]
fn ambiguous_malformed_and_excessive_username_forms_never_become_credentials() {
    for identity in [
        "username=\"alice\", username*=UTF-8''alice".to_owned(),
        "username*=UTF-8''alice, userhash=true".to_owned(),
        "username=\"alice\", userhash=true".to_owned(),
        "username=\"a77cfb90c6a66e542faaea176bec215b486ea0b9ea8b6a6449ff11d1efd4c2c\", userhash=true".to_owned(),
        "username=\"a77cfb90c6a66e542faaea176bec215b486ea0b9ea8b6a6449ff11d1efd4c2cg\", userhash=true".to_owned(),
        "username=\"alice\", userhash=unknown".to_owned(),
        "username=\"alice\", userhash=\"true\"".to_owned(),
        "username*=\"UTF-8''alice\"".to_owned(),
        "username*=UTF-8''".to_owned(),
        "username*=''alice".to_owned(),
        "username*=UTF-16''alice".to_owned(),
        "username*=UTF-8'bad_tag'alice".to_owned(),
        "username*=UTF-8'en--US'alice".to_owned(),
        "username*=UTF-8''alice%".to_owned(),
        "username*=UTF-8''alice%0".to_owned(),
        "username*=UTF-8''alice%GG".to_owned(),
        "username*=UTF-8''alice%00".to_owned(),
        "username*=UTF-8''alice%0A".to_owned(),
        "username*=UTF-8''alice%3Abob".to_owned(),
        "username*=UTF-8''alice%C0%AF".to_owned(),
        "username*=UTF-8''alice%ED%A0%80".to_owned(),
        "username*=UTF-8''alice*".to_owned(),
        format!("username*=UTF-8''{}", "a".repeat(257)),
        format!("username*=ISO-8859-1''{}", "%E9".repeat(129)),
    ] {
        assert!(Header::parse(&received(&identity)).and_then(|header| header.authorization().map(|_| ())).is_err(), "{identity}");
    }
}
