use crate::MAX_MESSAGE;
use rsipstack::sip::Request;

const MAX_HEADERS: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RequiredOptionsError {
    Malformed,
    Capacity,
}

pub(super) struct UnsupportedOptions {
    headers: String,
}

impl UnsupportedOptions {
    pub(super) fn response_headers(&self) -> &str {
        &self.headers
    }
}

pub(super) fn unsupported(
    request: &Request,
) -> Result<Option<UnsupportedOptions>, RequiredOptionsError> {
    if request.headers.len() > MAX_HEADERS {
        return Err(RequiredOptionsError::Capacity);
    }
    let mut headers = String::new();
    let mut bytes = 0_usize;
    for header in request
        .headers
        .iter()
        .filter(|header| header.name().eq_ignore_ascii_case("Require"))
    {
        let value = header.value();
        bytes = bytes
            .checked_add(value.len())
            .filter(|length| *length <= MAX_MESSAGE)
            .ok_or(RequiredOptionsError::Capacity)?;
        for token in value.split(',') {
            let token = token.trim_matches([' ', '\t']);
            if token.is_empty()
                || !token
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"-.!%*_+`'~".contains(&byte))
            {
                return Err(RequiredOptionsError::Malformed);
            }
            let separator = if headers.is_empty() {
                "Unsupported: "
            } else {
                ", "
            };
            if headers
                .len()
                .checked_add(separator.len())
                .and_then(|length| length.checked_add(token.len()))
                .and_then(|length| length.checked_add("\r\n".len()))
                .is_none_or(|length| length > MAX_MESSAGE)
            {
                return Err(RequiredOptionsError::Capacity);
            }
            headers.push_str(separator);
            headers.extend(
                token
                    .bytes()
                    .map(|byte| char::from(byte.to_ascii_lowercase())),
            );
        }
    }
    if headers.is_empty() {
        Ok(None)
    } else {
        headers.push_str("\r\n");
        Ok(Some(UnsupportedOptions { headers }))
    }
}
