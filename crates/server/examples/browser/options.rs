use std::{ffi::OsString, io};

pub fn port(arguments: impl IntoIterator<Item = OsString>) -> Result<u16, io::Error> {
    let mut arguments = arguments.into_iter();
    let first = arguments.next();
    if arguments.next().is_some() {
        return Err(io::Error::other(
            "browser example accepts one numeric localhost port",
        ));
    }
    let Some(first) = first else {
        return Ok(3000);
    };
    let text = first
        .to_str()
        .ok_or_else(|| io::Error::other("browser example port must contain ASCII digits"))?;
    if text.is_empty() || text.len() > 5 || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(io::Error::other(
            "browser example port must contain one to five ASCII digits",
        ));
    }
    text.parse().map_err(io::Error::other)
}
