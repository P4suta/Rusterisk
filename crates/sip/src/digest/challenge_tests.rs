use super::{Algorithm, Credentials, DigestError, DigestRequest, Header};

#[test]
fn received_challenges_choose_integrity_and_default_to_explicit_authentication(
) -> Result<(), DigestError> {
    for (parameters, expected) in [
        ("", "auth"),
        (", qop=\"auth\"", "auth"),
        (", qop=\"extension, auth, auth-int\"", "auth-int"),
    ] {
        let source = format!(
            "Digest realm=\"example.net\", nonce=\"server-nonce\", algorithm=SHA-256{parameters}"
        );
        let challenge = Header::parse(&source)?.challenge()?;
        assert_eq!(challenge.realm(), "example.net");
        assert_eq!(challenge.nonce(), "server-nonce");
        assert_eq!(challenge.algorithm(), Algorithm::Sha256);
        assert_eq!(challenge.quality(), expected);
        let credentials = Credentials::new("alice", "sample-password", challenge.realm())?;
        let request = DigestRequest::new(
            "REGISTER",
            "sip:registrar.example.net",
            challenge.nonce(),
            b"",
        )?;
        let authorization = challenge.authorization(&credentials, &request, "client-nonce", 1)?;
        assert!(authorization.contains(&format!("qop={expected}")));
        let header = Header::parse(&authorization)?;
        assert!(header
            .authorization()?
            .verify("REGISTER", b"", "sample-password")?);
    }
    Ok(())
}

#[test]
fn challenge_response_rendering_matches_independent_integrity_vectors() -> Result<(), DigestError> {
    for (algorithm, expected) in [
        ("MD5", "f0bf452c7251b9bba881f528119727bc"),
        ("MD5-sess", "32a331eff7e2c2da787affe8be67fd9c"),
        (
            "SHA-256",
            "877579be0585f470537b474714e1b081f147be3e64929fc7fd7feef79fa16404",
        ),
        (
            "SHA-512-256",
            "4f14eae7c8b01445d23ae8aa180efc7defaea2759b667fe68d486d34e036038e",
        ),
        (
            "SHA-256-sess",
            "7785d0f984b9d9fb6185806131d2c5fe5d226f680f7e584cecac64f949906d3a",
        ),
        (
            "SHA-512-256-sess",
            "3ed8634a2329ce28d942cfd49f453597879e8b410f90666f8314b7fbe11c7100",
        ),
    ] {
        let source = format!("Digest realm=\"example.net\", nonce=\"server-nonce\", algorithm={algorithm}, qop=\"auth-int\", opaque=\"roundtrip\", stale=true");
        let challenge = Header::parse(&source)?.challenge()?;
        assert!(challenge.stale());
        let credentials = Credentials::new("alice", "sample-password", challenge.realm())?;
        let request = DigestRequest::new(
            "REGISTER",
            "sip:registrar.example.net",
            challenge.nonce(),
            b"v=0\r\ns=call\r\n",
        )?;
        let authorization = challenge.authorization(&credentials, &request, "client-nonce", 1)?;
        assert!(authorization.contains(&format!("response=\"{expected}\"")));
        assert!(authorization.contains("opaque=\"roundtrip\""));
        assert!(authorization.contains(&format!("algorithm={algorithm}")));
        let header = Header::parse(&authorization)?;
        assert!(header.authorization()?.verify(
            "REGISTER",
            b"v=0\r\ns=call\r\n",
            "sample-password"
        )?);
        assert!(!header.authorization()?.verify(
            "REGISTER",
            b"v=0\r\ns=altered\r\n",
            "sample-password"
        )?);
    }
    Ok(())
}

#[test]
fn malformed_and_unsupported_challenges_never_become_legacy_authentication() {
    for source in [
        "Basic realm=\"example.net\"",
        "Digest realm=example.net, nonce=\"nonce\"",
        "Digest realm=\"example.net\", nonce=nonce",
        "Digest realm=\"example.net\", nonce=\"\"",
        "Digest realm=\"example.net\", nonce=\"nonce\", algorithm=SHA-512",
        "Digest realm=\"example.net\", nonce=\"nonce\", algorithm=\"SHA-256\"",
        "Digest realm=\"example.net\", nonce=\"nonce\", qop=auth",
        "Digest realm=\"example.net\", nonce=\"nonce\", qop=\"extension\"",
        "Digest realm=\"example.net\", nonce=\"nonce\", qop=\"auth,,auth-int\"",
        "Digest realm=\"example.net\", nonce=\"nonce\", stale=perhaps",
    ] {
        assert!(
            Header::parse(source)
                .and_then(|header| header.challenge())
                .is_err(),
            "{source}"
        );
    }
}

#[test]
fn challenge_values_roundtrip_once_and_keep_the_digest_uri_octets() -> Result<(), DigestError> {
    let challenge =
        Header::parse(r#"Digest realm="r\"e\\alm", nonce="n\"o\\nce", opaque="o\"p\\aque""#)?
            .challenge()?;
    assert_eq!(challenge.algorithm(), Algorithm::Md5);
    assert_eq!(challenge.quality(), "auth");
    let credentials = Credentials::new("a\"l\\ice", "p\"ass\\word", challenge.realm())?;
    let uri = "sip:%61LICE@EXAMPLE.net;transport=UDP";
    let request = DigestRequest::new("REGISTER", uri, challenge.nonce(), b"")?;
    let wire = challenge.authorization(&credentials, &request, "client-nonce", 1)?;
    let parsed = Header::parse(&wire)?;
    let authorization = parsed.authorization()?;
    assert_eq!(authorization.username(), "a\"l\\ice");
    assert_eq!(authorization.realm(), "r\"e\\alm");
    assert_eq!(authorization.nonce(), "n\"o\\nce");
    assert_eq!(authorization.uri(), uri);
    assert_eq!(parsed.get("opaque"), Some("o\"p\\aque"));
    assert!(authorization.verify("REGISTER", b"", "p\"ass\\word")?);
    Ok(())
}

#[test]
fn unsupported_profiles_and_invalid_boundaries_cannot_emit_authorization() -> Result<(), DigestError>
{
    for source in [
        "Digest realm=\"example.net\", nonce=\"nonce\", charset=ISO-8859-1".to_owned(),
        "Digest realm=\"example.net\", nonce=\"nonce\", userhash=unknown".to_owned(),
        format!("Digest realm=\"{}\", nonce=\"nonce\"", "x".repeat(257)),
        format!(
            "Digest realm=\"example.net\", nonce=\"{}\"",
            "x".repeat(257)
        ),
        format!(
            "Digest realm=\"example.net\", nonce=\"nonce\", opaque=\"{}\"",
            "x".repeat(257)
        ),
        "Digest realm=\"example.net\", nonce=\"nonce\", qop=\"a,b,c,d,e,f,g,h,auth\"".to_owned(),
    ] {
        assert!(Header::parse(&source)
            .and_then(|header| header.challenge())
            .is_err());
    }
    let challenge = Header::parse("Digest realm=\"example.net\", nonce=\"nonce\"")?.challenge()?;
    let credentials = Credentials::new("alice", "sample-password", "example.net")?;
    let request = DigestRequest::new("REGISTER", "sip:registrar.example.net", "nonce", b"")?;
    assert!(challenge
        .authorization(&credentials, &request, "client", 0)
        .is_err());
    assert!(challenge
        .authorization(&credentials, &request, "", 1)
        .is_err());
    let other_realm = Credentials::new("alice", "sample-password", "another-provider")?;
    assert!(challenge
        .authorization(&other_realm, &request, "client", 1)
        .is_err());
    let other_nonce =
        DigestRequest::new("REGISTER", "sip:registrar.example.net", "other-nonce", b"")?;
    assert!(challenge
        .authorization(&credentials, &other_nonce, "client", 1)
        .is_err());
    Ok(())
}
