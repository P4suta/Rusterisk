use super::{attribute, authorization, authorized, challenge, engine, originate, sha512256};
use rusterisk_sip::SipError;

#[test]
fn alternating_invite_scopes_preserve_each_negotiated_username_form() -> Result<(), SipError> {
    for proxy_first in [false, true] {
        let mut engine = engine()?;
        let initial = originate(&mut engine)?;
        let first = engine.receive(
            &challenge(&initial, proxy_first, &["Digest realm=\"provider.example\", nonce=\"first-nonce\", algorithm=SHA-512-256, qop=\"auth-int\", charset=UTF-8, userhash=true"] )?,
            "127.0.0.1:5061".parse()?,
            1,
        )?;
        let first_request = authorized(&first)?;
        let client = attribute(authorization(&first_request, proxy_first)?, "cnonce")?;
        let second = engine.receive(
            &challenge(&first.datagrams[1].bytes, !proxy_first, &["Digest realm=\"provider.example\", nonce=\"second-nonce\", algorithm=SHA-256, qop=\"auth\", userhash=false"] )?,
            "127.0.0.1:5061".parse()?,
            2,
        )?;
        let request = authorized(&second)?;
        let hashed = authorization(&request, proxy_first)?;
        assert_eq!(
            attribute(hashed, "username")?,
            sha512256("outbound:provider.example")
        );
        assert_eq!(attribute(hashed, "userhash")?, "true");
        assert_eq!(attribute(hashed, "nc")?, "00000002");
        let identity = sha512256("outbound:provider.example:remote-password");
        let entity = sha512256(&request.body);
        let target = sha512256(format!("INVITE:{}:{entity}", request.uri));
        let expected = sha512256(format!(
            "{identity}:first-nonce:00000002:{client}:auth-int:{target}"
        ));
        assert_eq!(attribute(hashed, "response")?, expected);
        let plain = authorization(&request, !proxy_first)?;
        assert_eq!(attribute(plain, "username")?, "outbound");
        assert!(!plain.contains("userhash=true"));
    }
    Ok(())
}
