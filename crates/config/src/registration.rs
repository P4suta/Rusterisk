use crate::{ConfigError, Section};

pub(crate) fn contact_user(file: &str, section: &Section) -> Result<String, ConfigError> {
    let value = section.value("contact_user").unwrap_or("s");
    if value.is_empty() || value.len() > 256 || !valid_user(value) {
        return Err(ConfigError::at(
            file,
            section.position("contact_user"),
            "contact_user must be an ASCII SIP URI user of at most 256 bytes",
        ));
    }
    Ok(value.into())
}

fn valid_user(value: &str) -> bool {
    let mut bytes = value.bytes();
    while let Some(byte) = bytes.next() {
        if byte == b'%' {
            if !bytes.next().is_some_and(|byte| byte.is_ascii_hexdigit())
                || !bytes.next().is_some_and(|byte| byte.is_ascii_hexdigit())
            {
                return false;
            }
        } else if !byte.is_ascii_alphanumeric() && !b"-_.!~*'()&=+$,;?/".contains(&byte) {
            return false;
        }
    }
    true
}
