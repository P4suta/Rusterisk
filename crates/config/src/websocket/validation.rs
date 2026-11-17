pub(super) struct InvalidClient {
    pub(super) field: &'static str,
    pub(super) reason: &'static str,
}

fn invalid(field: &'static str, reason: &'static str) -> InvalidClient {
    InvalidClient { field, reason }
}

pub(super) fn name(value: &str) -> Result<(), InvalidClient> {
    if !crate::valid_name(value) || value.contains('/') || matches!(value, "." | "..") {
        Err(invalid("name", "invalid media connection id"))
    } else {
        Ok(())
    }
}

pub(super) fn uri(value: &str) -> Result<url::Url, InvalidClient> {
    let url = url::Url::parse(value).map_err(|_| invalid("uri", "invalid media URI"))?;
    if !matches!(url.scheme(), "ws" | "wss")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(invalid("uri", "invalid media URI"));
    }
    Ok(url)
}

pub(super) fn ca(path: Option<&str>, encrypted: bool) -> Result<(), InvalidClient> {
    if path.is_some_and(|path| {
        !encrypted || path.is_empty() || path.len() > 4096 || path.chars().any(char::is_control)
    }) {
        Err(invalid("ca_list_file", "invalid TLS trust file"))
    } else {
        Ok(())
    }
}

pub(super) fn credentials(
    username: Option<&str>,
    password: Option<&str>,
) -> Result<(), InvalidClient> {
    if username.is_some() != password.is_some() {
        return Err(invalid(
            if password.is_some() {
                "password"
            } else {
                "username"
            },
            "username and password must be paired",
        ));
    }
    if username.is_some_and(|name| {
        name.is_empty()
            || name.len() > 128
            || name.contains(':')
            || name.chars().any(char::is_control)
    }) {
        return Err(invalid("username", "invalid media username"));
    }
    if password.is_some_and(|password| {
        password.is_empty() || password.len() > 256 || password.chars().any(char::is_control)
    }) {
        return Err(invalid("password", "invalid media password"));
    }
    Ok(())
}

pub(super) fn timing(timeout: u16, interval: u16, attempts: u16) -> Result<(), InvalidClient> {
    if timeout == 0 || timeout > 4000 {
        return Err(invalid(
            "connection_timeout",
            "connection_timeout must be between 1 and 4000 milliseconds",
        ));
    }
    if interval == 0 || interval > 4000 {
        return Err(invalid(
            "reconnect_interval",
            "reconnect_interval must be between 1 and 4000 milliseconds",
        ));
    }
    if !(1..=4).contains(&attempts) {
        return Err(invalid(
            "reconnect_attempts",
            "reconnect_attempts must be between 1 and 4",
        ));
    }
    let total =
        u32::from(timeout) * u32::from(attempts) + u32::from(interval) * u32::from(attempts - 1);
    if total > 4000 {
        return Err(invalid(
            "connection_timeout",
            "media connection attempts exceed the four-second admission budget",
        ));
    }
    Ok(())
}
