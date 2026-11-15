use super::Header;
use crate::digest::DigestError;

fn authorization(extra: &str) -> String {
    format!(
        "Digest username=\"alice\", realm=\"example.net\", nonce=\"server-nonce\", uri=\"sip:registrar.example.net\", response=\"877579be0585f470537b474714e1b081f147be3e64929fc7fd7feef79fa16404\", algorithm=SHA-256, qop=auth-int, cnonce=\"client-nonce\", nc=00000001{extra}"
    )
}

#[test]
fn integrity_authorization_preserves_the_entity_and_decodes_its_response() -> Result<(), DigestError>
{
    let header = Header::parse(&authorization(""))?;
    let received = header.authorization()?;
    assert_eq!(received.username(), "alice");
    assert_eq!(received.realm(), "example.net");
    assert_eq!(received.nonce(), "server-nonce");
    assert_eq!(received.uri(), "sip:registrar.example.net");
    assert_eq!(received.count(), 1);
    assert!(received.verify("REGISTER", b"v=0\r\ns=call\r\n", "sample-password")?);
    assert!(!received.verify("REGISTER", b"v=0\r\ns=changed\r\n", "sample-password")?);
    assert!(!received.verify("REGISTER", b"", "sample-password")?);
    assert!(!received.verify("INVITE", b"v=0\r\ns=call\r\n", "sample-password")?);
    Ok(())
}

#[test]
fn quoted_pairs_decode_once_and_request_uri_octets_remain_unchanged() -> Result<(), DigestError> {
    let input = authorization("")
        .replace("realm=\"example.net\"", r#"realm="r\"e\\alm""#)
        .replace(
            "sip:registrar.example.net",
            "sip:ALICE@EXAMPLE.net;transport=UDP",
        );
    let header = Header::parse(&input)?;
    assert_eq!(header.get("REALM"), Some("r\"e\\alm"));
    assert_eq!(
        header.authorization()?.uri(),
        "sip:ALICE@EXAMPLE.net;transport=UDP"
    );
    Ok(())
}

#[test]
fn ambiguous_attributes_and_broken_framing_are_rejected() {
    for input in [
        authorization(", nonce=\"second\""),
        authorization(", ALGORITHM=MD5"),
        authorization(", opaque=\"a\", OPAQUE=\"b\""),
        authorization("").replace(", realm", " realm"),
        authorization("").replace("nc=00000001", "nc=00000001 trailing"),
        authorization("").replace("nonce=\"server-nonce\"", "nonce=\"unterminated"),
        authorization(", bad key=value"),
        authorization(", extension="),
        authorization(","),
        authorization(", injected=\"x\r\nHeader: value\""),
    ] {
        assert!(
            Header::parse(&input)
                .and_then(|header| header.authorization().map(|_| ()))
                .is_err(),
            "{input}"
        );
    }
}

#[test]
fn nonce_counts_and_unsupported_algorithms_are_not_silently_normalized() -> Result<(), DigestError>
{
    for count in ["1", "00000000", "000000001", "0000000x"] {
        let input = authorization("").replace("nc=00000001", &format!("nc={count}"));
        assert!(Header::parse(&input)?.authorization().is_err());
    }
    for input in [
        authorization("").replace("algorithm=SHA-256", "algorithm=SHA-512-256"),
        authorization("").replace("qop=auth-int", "qop=unknown"),
        authorization(", userhash=true"),
        authorization(", username*=UTF-8''alice"),
        authorization("").replacen("Digest ", "Basic ", 1),
    ] {
        assert!(matches!(
            Header::parse(&input).and_then(|header| header.authorization().map(|_| ())),
            Err(DigestError::Unsupported)
        ));
    }
    Ok(())
}

#[test]
fn attribute_storage_and_wire_text_have_finite_limits() -> Result<(), DigestError> {
    let mut source = authorization("");
    for index in 0..7 {
        source.push_str(&format!(", extension{index}=value"));
    }
    assert!(Header::parse(&source)?.authorization().is_ok());
    source.push_str(", extension7=value");
    assert!(Header::parse(&source).is_err());
    assert!(Header::parse(&format!("Digest realm=\"{}\"", "r".repeat(4096))).is_err());
    assert!(Header::parse(&authorization(", opaque=\"ok\""))?
        .authorization()
        .is_ok());
    Ok(())
}
