use crate::{
    digest::DigestError, registration_client::RegistrationIdentity, SipError, MAX_MESSAGE,
};

pub(super) struct RequestUpdate<'a> {
    pub(super) identity: RegistrationIdentity,
    pub(super) expiration: Option<u32>,
    pub(super) authorization: Option<&'a str>,
    pub(super) proxy_authorization: Option<&'a str>,
}

pub(super) fn header<'a>(source: &'a str, name: &str) -> Option<&'a str> {
    source
        .split_once("\r\n\r\n")?
        .0
        .split("\r\n")
        .filter_map(|line| line.split_once(':'))
        .find_map(|(field, value)| {
            field
                .eq_ignore_ascii_case(name)
                .then(|| value.trim_matches([' ', '\t']))
        })
}

pub(super) fn expiration<'a>(
    values: impl IntoIterator<Item = &'a str>,
) -> Result<Option<u32>, SipError> {
    let mut values = values.into_iter();
    let Some(value) = values.next() else {
        return Ok(None);
    };
    if values.next().is_some()
        || value.is_empty()
        || !value.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(SipError::InvalidPacket);
    }
    value.parse().map(Some).map_err(|_| SipError::InvalidPacket)
}

pub(super) fn render(source: &str, update: RequestUpdate<'_>) -> Result<Vec<u8>, SipError> {
    let (headers, body) = source
        .split_once("\r\n\r\n")
        .ok_or(SipError::InvalidPacket)?;
    let mut output = String::with_capacity(source.len().min(MAX_MESSAGE));
    for (index, line) in headers.split("\r\n").enumerate() {
        if index == 0 {
            output.push_str(line);
        } else {
            let (name, value) = line.split_once(':').ok_or(SipError::InvalidPacket)?;
            if name.eq_ignore_ascii_case("Authorization")
                || name.eq_ignore_ascii_case("Proxy-Authorization")
            {
                continue;
            }
            if name.eq_ignore_ascii_case("Via") {
                let (sent_by, _) = value
                    .rsplit_once(";branch=")
                    .ok_or(SipError::InvalidPacket)?;
                output.push_str(&format!(
                    "Via:{sent_by};branch=z9hG4bK-reg-{:032x}-{}",
                    update.identity.branch, update.identity.sequence
                ));
            } else if name.eq_ignore_ascii_case("CSeq") {
                output.push_str(&format!("CSeq: {} REGISTER", update.identity.sequence));
            } else if let Some(expiration) = update
                .expiration
                .filter(|_| name.eq_ignore_ascii_case("Expires"))
            {
                output.push_str(&format!("Expires: {expiration}"));
            } else {
                output.push_str(line);
            }
        }
        output.push_str("\r\n");
    }
    for (name, value) in [
        ("Authorization", update.authorization),
        ("Proxy-Authorization", update.proxy_authorization),
    ] {
        if let Some(value) = value {
            output.push_str(name);
            output.push_str(": ");
            output.push_str(value);
            output.push_str("\r\n");
        }
    }
    output.push_str("\r\n");
    output.push_str(body);
    if output.len() > MAX_MESSAGE {
        return Err(SipError::Capacity);
    }
    Ok(output.into_bytes())
}

pub(super) fn digest_error(error: DigestError) -> SipError {
    match error {
        DigestError::Invalid => SipError::InvalidPacket,
        DigestError::Unsupported => SipError::Unsupported,
    }
}
