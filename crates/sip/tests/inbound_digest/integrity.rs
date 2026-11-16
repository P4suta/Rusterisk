use super::{attribute, hash, Name};
use rusterisk_sip::SipError;

pub(super) fn authorization(
    offer: &str,
    username: &str,
    password: &str,
    name: Name<'_>,
    method: &str,
    uri: &str,
    body: &str,
) -> Result<String, SipError> {
    let algorithm = attribute(offer, "algorithm")?;
    let nonce = attribute(offer, "nonce")?;
    let realm = attribute(offer, "realm")?;
    let identity = hash(algorithm, &format!("{username}:{realm}:{password}"))?;
    let entity = hash(algorithm, body)?;
    let target = hash(algorithm, &format!("{method}:{uri}:{entity}"))?;
    let digest = hash(
        algorithm,
        &format!("{identity}:{nonce}:00000001:fixture-client:auth-int:{target}"),
    )?;
    let name = match name {
        Name::Plain => format!("username=\"{username}\""),
        Name::Hashed => format!(
            "username=\"{}\", userhash=true",
            hash(algorithm, &format!("{username}:{realm}"))?
        ),
        Name::Extended(value) => format!("username*={value}, userhash=false"),
    };
    Ok(format!("Authorization: Digest {name}, realm=\"{realm}\", nonce=\"{nonce}\", uri=\"{uri}\", response=\"{digest}\", algorithm={algorithm}, qop=auth-int, nc=00000001, cnonce=\"fixture-client\"\r\n"))
}
