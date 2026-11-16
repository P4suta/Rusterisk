use super::{
    attribute, authorization, authorized, challenge, engine_with_password, originate, sha256,
};
use rusterisk_sip::SipError;

#[test]
fn alternating_invite_scopes_preserve_each_negotiated_credential_encoding() -> Result<(), SipError>
{
    for proxy_first in [false, true] {
        let mut engine = engine_with_password("pa\u{030a}ss")?;
        let initial = originate(&mut engine)?;
        let first = engine.receive(
            &challenge(
                &initial,
                proxy_first,
                &["Digest realm=\"provider.example\", nonce=\"utf8-nonce\", algorithm=SHA-256, qop=\"auth\", charset=UTF-8"],
            )?,
            "127.0.0.1:5061".parse()?,
            1,
        )?;
        let first_request = authorized(&first)?;
        let first_authorization = authorization(&first_request, proxy_first)?;
        let client = attribute(first_authorization, "cnonce")?;
        let second = engine.receive(
            &challenge(
                &first.datagrams[1].bytes,
                !proxy_first,
                &["Digest realm=\"provider.example\", nonce=\"raw-nonce\", algorithm=SHA-256, qop=\"auth\""],
            )?,
            "127.0.0.1:5061".parse()?,
            2,
        )?;
        let request = authorized(&second)?;
        let target = sha256(format!("INVITE:{}", request.uri));
        let identity = sha256("outbound:provider.example:p\u{00e5}ss");
        let expected = sha256(format!(
            "{identity}:utf8-nonce:00000002:{client}:auth:{target}"
        ));
        let renewed = authorization(&request, proxy_first)?;
        assert_eq!(attribute(renewed, "response")?, expected);
        assert_eq!(attribute(renewed, "nc")?, "00000002");
        assert!(!renewed.contains("charset="));

        let added = authorization(&request, !proxy_first)?;
        let client = attribute(added, "cnonce")?;
        let identity = sha256("outbound:provider.example:pa\u{030a}ss");
        let expected = sha256(format!(
            "{identity}:raw-nonce:00000001:{client}:auth:{target}"
        ));
        assert_eq!(attribute(added, "response")?, expected);
        assert_eq!(attribute(added, "nc")?, "00000001");
    }
    Ok(())
}
