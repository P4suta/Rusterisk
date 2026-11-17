use crate::{
    Auth, ConfigError, Protocol, RegistrarUri, Registration, RegistrationAddress, Section,
    Transport,
};

pub(crate) fn parse(
    file: &str,
    section: &Section,
    transports: &[Transport],
    auth: &[Auth],
) -> Result<Registration, ConfigError> {
    let required = |key| {
        section
            .value(key)
            .ok_or_else(|| ConfigError::at(file, section.position(key), format!("missing {key}")))
    };
    let transport_name = required("transport")?;
    let transport = transports
        .iter()
        .find(|transport| transport.name == transport_name)
        .ok_or_else(|| {
            ConfigError::at(
                file,
                section.position("transport"),
                "unresolved registration transport",
            )
        })?;
    let outbound_auth = required("outbound_auth")?;
    if !auth.iter().any(|auth| auth.name == outbound_auth) {
        return Err(ConfigError::at(
            file,
            section.position("outbound_auth"),
            "unresolved registration outbound_auth",
        ));
    }
    let server_uri =
        RegistrarUri::parse(transport.protocol, required("server_uri")?).map_err(|error| {
            ConfigError::at(file, section.position("server_uri"), error.to_string())
        })?;
    let client_uri = RegistrationAddress::parse(required("client_uri")?).map_err(|error| {
        ConfigError::at(file, section.position("client_uri"), error.to_string())
    })?;
    validate_roles(transport.protocol, &server_uri, &client_uri)
        .map_err(|issue| ConfigError::at(file, section.position(issue.option()), issue.reason()))?;
    Ok(Registration {
        name: section.name.clone(),
        transport: transport_name.into(),
        outbound_auth: outbound_auth.into(),
        server_uri,
        client_uri,
        expiration: section.number(file, "expiration", 3600)?,
        contact_user: contact_user(file, section)?,
        retry_interval: section.number(file, "retry_interval", 60)?,
        forbidden_retry_interval: section.number(file, "forbidden_retry_interval", 0)?,
        fatal_retry_interval: section.number(file, "fatal_retry_interval", 0)?,
        max_retries: section.number(file, "max_retries", 10)?,
        max_random_initial_delay: section.number(file, "max_random_initial_delay", 10)?,
        auth_rejection_permanent: section.flag(file, "auth_rejection_permanent", true)?,
    })
}

fn contact_user(file: &str, section: &Section) -> Result<String, ConfigError> {
    let value = section.value("contact_user").unwrap_or("s");
    if !valid_contact_user(value) {
        return Err(ConfigError::at(
            file,
            section.position("contact_user"),
            "contact_user must be an ASCII SIP URI user of at most 256 bytes",
        ));
    }
    Ok(value.into())
}

pub(super) enum RoleIssue {
    ServerTransport,
    ClientTransport,
    RegistrarRequiresSecureClient,
    ContactRequiresSecureRegistrar,
}

impl RoleIssue {
    pub(super) const fn option(&self) -> &'static str {
        match self {
            Self::ServerTransport | Self::ContactRequiresSecureRegistrar => "server_uri",
            Self::ClientTransport | Self::RegistrarRequiresSecureClient => "client_uri",
        }
    }

    pub(super) const fn reason(&self) -> &'static str {
        match self {
            Self::ServerTransport | Self::ClientTransport => {
                "registration URI conflicts with its transport"
            }
            Self::RegistrarRequiresSecureClient => {
                "SIPS registrar scheme requires SIPS Contact and client_uri schemes"
            }
            Self::ContactRequiresSecureRegistrar => {
                "SIPS Contact scheme requires the registrar Request-URI scheme to be SIPS"
            }
        }
    }
}

pub(super) fn validate_roles(
    protocol: Protocol,
    server_uri: &RegistrarUri,
    client_uri: &RegistrationAddress,
) -> Result<(), RoleIssue> {
    if server_uri.protocol() != protocol {
        return Err(RoleIssue::ServerTransport);
    }
    if client_uri.is_secure() && protocol != Protocol::Tls {
        return Err(RoleIssue::ClientTransport);
    }
    match (server_uri.is_secure(), client_uri.is_secure()) {
        (true, false) => Err(RoleIssue::RegistrarRequiresSecureClient),
        (false, true) => Err(RoleIssue::ContactRequiresSecureRegistrar),
        (false, false) | (true, true) => Ok(()),
    }
}

pub(super) fn valid_contact_user(value: &str) -> bool {
    !value.is_empty() && value.len() <= 256 && valid_user(value)
}

pub(super) fn valid_user(value: &str) -> bool {
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
