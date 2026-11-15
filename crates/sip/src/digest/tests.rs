use super::{Algorithm, Credentials, DigestError, DigestRequest, Protection, RequestUri};

fn protection() -> Result<Protection<'static>, DigestError> {
    Protection::parse(Some("auth-int"), Some("client-nonce"), Some("00000001"))
}

#[test]
fn http_digest_reference_vectors_match_both_algorithms() -> Result<(), DigestError> {
    let credentials = Credentials::new("Mufasa", "Circle of Life", "http-auth@example.org")?;
    let request = DigestRequest {
        method: "GET",
        uri: RequestUri("/dir/index.html"),
        nonce: super::Nonce::new("7ypf/xlj9XXwfDPEoM4URrv/xwf94BcCAzFZH4GiTo0v")?,
        body: b"",
    };
    let protection = Protection::parse(
        Some("auth"),
        Some("f2/wE4q74E6zIJEtWaHKaf5wv/H5QzzpXusqGemxURZJ"),
        Some("00000001"),
    )?;
    for (name, expected) in [
        ("MD5", "8ca523f5e9506fed4657c9700eebdbec"),
        (
            "SHA-256",
            "753927fa0e85d155564e2e272a28d1802ca10daf4496794697cf8db5856cb6c1",
        ),
    ] {
        let algorithm = Algorithm::parse(Some(name))?;
        let response = algorithm.calculate(&credentials, &request, &protection)?;
        assert_eq!(response.hex(), expected, "{name}");
        assert!(response.matches(expected));
        assert!(!response.matches(&format!("{expected}0")));
        assert!(!response.matches(&"0".repeat(expected.len())));
    }
    Ok(())
}

#[test]
fn openssl_integrity_and_session_vectors_use_the_selected_body_hash() -> Result<(), DigestError> {
    let credentials = Credentials::new("alice", "sample-password", "example.net")?;
    for (name, empty, body) in [
        (
            "MD5",
            "0c812f0dd231ebc2bb796f3b7db9fa3d",
            "f0bf452c7251b9bba881f528119727bc",
        ),
        (
            "MD5-sess",
            "eadfd62d16bb9eca39d035f7bef1979c",
            "32a331eff7e2c2da787affe8be67fd9c",
        ),
        (
            "SHA-256",
            "a792eca85ef0f834b4067fd943f282a80ab0910e17e711220b20eed0acb8b551",
            "877579be0585f470537b474714e1b081f147be3e64929fc7fd7feef79fa16404",
        ),
        (
            "SHA-256-sess",
            "f6bda331170cb5610a37a89235a5a079da49b5af1aa3112d321b293459854bf2",
            "7785d0f984b9d9fb6185806131d2c5fe5d226f680f7e584cecac64f949906d3a",
        ),
    ] {
        let algorithm = Algorithm::parse(Some(name))?;
        for (entity, expected) in [(b"".as_slice(), empty), (b"v=0\r\ns=call\r\n", body)] {
            let request = DigestRequest::new(
                "REGISTER",
                "sip:registrar.example.net",
                "server-nonce",
                entity,
            )?;
            assert_eq!(
                algorithm
                    .calculate(&credentials, &request, &protection()?)?
                    .hex(),
                expected,
                "{name} {entity:?}"
            );
        }
    }
    Ok(())
}

#[test]
fn unsupported_algorithms_never_become_the_legacy_default() -> Result<(), DigestError> {
    assert_eq!(Algorithm::parse(None)?.name(), "MD5");
    for source in ["SHA-384", "SHA-512", "", "MD5, SHA-256", "unknown"] {
        assert_eq!(
            Algorithm::parse(Some(source)),
            Err(DigestError::Unsupported)
        );
    }
    assert_eq!(Algorithm::parse(Some("sha-256"))?.name(), "SHA-256");
    assert_eq!(
        Algorithm::parse(Some("sHa-256-SeSs"))?.name(),
        "SHA-256-sess"
    );
    Ok(())
}

#[test]
fn protection_requires_a_complete_nonzero_nonce_count() {
    for (qop, cnonce, count) in [
        (Some("auth"), None, Some("00000001")),
        (Some("auth"), Some("c"), None),
        (Some("auth"), Some(""), Some("00000001")),
        (Some("auth"), Some("c"), Some("00000000")),
        (Some("auth-int"), Some("c"), Some("1")),
        (Some("auth-int"), Some("c"), Some("100000000")),
        (Some("auth-int"), Some("c"), Some("0000000g")),
        (None, Some("c"), Some("00000001")),
    ] {
        assert!(Protection::parse(qop, cnonce, count).is_err());
    }
    assert!(matches!(
        Protection::parse(Some("unsupported"), Some("c"), Some("00000001")),
        Err(DigestError::Unsupported)
    ));
    assert!(Protection::parse(Some("auth"), Some("c"), Some("ffffffff")).is_ok());
}

#[test]
fn invalid_or_excessive_inputs_do_not_reach_digest_calculation() {
    for uri in [
        "",
        "http://example.net",
        "sip:",
        "sip:example.net\r\nX: injected",
    ] {
        assert!(DigestRequest::new("REGISTER", uri, "nonce", b"").is_err());
    }
    assert!(DigestRequest::new("REGISTER\n", "sip:example.net", "nonce", b"").is_err());
    assert!(DigestRequest::new("REGISTER", "sip:example.net", "", b"").is_err());
    assert!(DigestRequest::new("REGISTER", "sip:example.net", "n\n", b"").is_err());
    assert!(Credentials::new("alice:bob", "password", "realm").is_err());
    assert!(Credentials::new("alice", "password", "r\r\n").is_err());
    assert!(Credentials::new(&"a".repeat(257), "password", "realm").is_err());
    let entity = [0; crate::MAX_MESSAGE + 1];
    assert!(DigestRequest::new("REGISTER", "sip:example.net", "nonce", &entity).is_err());
    assert!(Protection::parse(Some("auth"), Some(&"c".repeat(257)), Some("00000001")).is_err());
}

#[test]
fn session_algorithms_require_a_client_nonce() -> Result<(), DigestError> {
    let credentials = Credentials::new("alice", "sample-password", "example.net")?;
    let request = DigestRequest::new("REGISTER", "sip:registrar.example.net", "server-nonce", b"")?;
    for name in ["MD5-sess", "SHA-256-sess", "SHA-512-256-sess"] {
        assert!(matches!(
            Algorithm::parse(Some(name))?.calculate(
                &credentials,
                &request,
                &Protection::parse(None, None, None)?
            ),
            Err(DigestError::Invalid)
        ));
    }
    Ok(())
}

#[test]
fn openssl_sha512_256_vectors_require_the_standard_initial_value_and_session_derivation(
) -> Result<(), DigestError> {
    let credentials = Credentials::new("Jäsøn Doe", "Secret, or not?", "api@example.org")?;
    let request = DigestRequest {
        method: "GET",
        uri: RequestUri("/doe.json"),
        nonce: super::Nonce::new("5TsQWLVdgBdmrQ0XsxbDODV+57QdFR34I9HAbC/RVvkK")?,
        body: b"",
    };
    let quality = Protection::parse(
        Some("auth"),
        Some("NTg6RKcb9boFIAS3KrFK9BGeh+iDa/sm6jUMp2wds69v"),
        Some("00000001"),
    )?;
    let response =
        Algorithm::parse(Some("SHA-512-256"))?.calculate(&credentials, &request, &quality)?;
    assert_eq!(
        response.hex(),
        "3798d4131c277846293534c3edc11bd8a5e4cdcbff78b05db9d95eeb1cec68a5"
    );
    assert!(!response.matches("ae66e67d6b427bd3f120414a82e4acff38e8ecd9101d6c861229025f607a79dd"));
    let credentials = Credentials::new("alice", "sample-password", "example.net")?;
    for (algorithm, entity, expected) in [
        (
            "SHA-512-256",
            b"".as_slice(),
            "4cc0e017d37db99606dbfac1abc1c4da126adee45416adeb7e747a604abb0480",
        ),
        (
            "SHA-512-256",
            b"v=0\r\ns=call\r\n",
            "4f14eae7c8b01445d23ae8aa180efc7defaea2759b667fe68d486d34e036038e",
        ),
        (
            "SHA-512-256-sess",
            b"".as_slice(),
            "ff4fafa5018fa9edf6472cae3933e68ca21ae9d04da8203e28d4c6c5fb951152",
        ),
        (
            "SHA-512-256-sess",
            b"v=0\r\ns=call\r\n",
            "3ed8634a2329ce28d942cfd49f453597879e8b410f90666f8314b7fbe11c7100",
        ),
    ] {
        let request = DigestRequest::new(
            "REGISTER",
            "sip:registrar.example.net",
            "server-nonce",
            entity,
        )?;
        assert_eq!(
            Algorithm::parse(Some(algorithm))?
                .calculate(&credentials, &request, &protection()?)?
                .hex(),
            expected
        );
    }
    Ok(())
}
