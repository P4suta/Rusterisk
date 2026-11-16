use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr};
use std::path::Path;
mod expansion;
mod ini_comments;
mod registration;
mod registration_uri;
mod wait_duration;
mod websocket;
pub use expansion::{expand_variables, ExpansionError};
pub use registration_uri::{RegistrarUri, RegistrationAddress, RegistrationUriError};
pub use wait_duration::{InvalidWaitDuration, WaitDuration};
pub use websocket::{WebSocketClient, WebSocketConfig, WebSocketControlFormat};

const MAX_FILE: usize = 1024 * 1024;
const MAX_SECTIONS: usize = 256;
const MAX_OPTIONS: usize = 128;
pub const MAX_TRANSPORTS: usize = 16;
pub const MAX_REGISTRATIONS: usize = 16;

#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[redacted]")
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigError {
    pub file: String,
    pub line: usize,
    pub reason: String,
}

impl ConfigError {
    fn at(file: &str, line: usize, reason: impl Into<String>) -> Self {
        Self {
            file: file.into(),
            line,
            reason: reason.into(),
        }
    }
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}: {}", self.file, self.line, self.reason)
    }
}

impl std::error::Error for ConfigError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ini {
    pub sections: Vec<Section>,
}

#[derive(Clone, Copy)]
enum SectionIdentity {
    Name,
    NameAndType,
}

#[derive(Clone, PartialEq, Eq)]
pub struct Section {
    pub name: String,
    pub line: usize,
    options: BTreeMap<String, (usize, String)>,
}

impl std::fmt::Debug for Section {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Section")
            .field("name", &self.name)
            .field("line", &self.line)
            .finish_non_exhaustive()
    }
}

impl Ini {
    pub fn parse(file: &str, source: &str) -> Result<Self, ConfigError> {
        Self::parse_sections(file, source, SectionIdentity::Name)
    }

    fn parse_signaling(file: &str, source: &str) -> Result<Self, ConfigError> {
        Self::parse_sections(file, source, SectionIdentity::NameAndType)
    }

    fn parse_sections(
        file: &str,
        source: &str,
        identity: SectionIdentity,
    ) -> Result<Self, ConfigError> {
        if source.len() > MAX_FILE {
            return Err(ConfigError::at(file, 1, "file exceeds 1 MiB"));
        }
        let mut sections: Vec<Section> = Vec::new();
        for (index, raw) in source.lines().enumerate() {
            let number = index + 1;
            if raw.len() > 4096 {
                return Err(ConfigError::at(file, number, "line exceeds 4096 bytes"));
            }
            let content = ini_comments::content(raw);
            let line = content.trim();
            if line.is_empty() {
                continue;
            }
            if line.starts_with('#') {
                return Err(ConfigError::at(
                    file,
                    number,
                    "configuration directives are unsupported",
                ));
            }
            if line.starts_with('[') {
                let name = line
                    .strip_prefix('[')
                    .and_then(|text| text.strip_suffix(']'))
                    .ok_or_else(|| ConfigError::at(file, number, "invalid section header"))?;
                if !valid_name(name) || sections.len() >= MAX_SECTIONS {
                    return Err(ConfigError::at(
                        file,
                        number,
                        "invalid section name or section capacity exceeded",
                    ));
                }
                if matches!(identity, SectionIdentity::Name)
                    && sections.iter().any(|section| section.name == name)
                {
                    return Err(ConfigError::at(file, number, "duplicate section"));
                }
                sections.push(Section {
                    name: name.into(),
                    line: number,
                    options: BTreeMap::new(),
                });
            } else {
                let (key, value) = line
                    .split_once('=')
                    .ok_or_else(|| ConfigError::at(file, number, "expected key=value"))?;
                let section = sections
                    .last_mut()
                    .ok_or_else(|| ConfigError::at(file, number, "option outside a section"))?;
                let key = key.trim();
                if !valid_name(key) || section.options.len() >= MAX_OPTIONS {
                    return Err(ConfigError::at(
                        file,
                        number,
                        "invalid option or option capacity exceeded",
                    ));
                }
                if section
                    .options
                    .insert(key.into(), (number, value.trim().into()))
                    .is_some()
                {
                    return Err(ConfigError::at(file, number, "duplicate option"));
                }
            }
        }
        match identity {
            SectionIdentity::Name => {}
            SectionIdentity::NameAndType => {
                for (index, section) in sections.iter().enumerate() {
                    let kind = section.required(file, "type")?;
                    if sections[..index].iter().any(|previous| {
                        previous.name == section.name && previous.value("type") == Some(kind)
                    }) {
                        return Err(ConfigError::at(
                            file,
                            section.line,
                            "duplicate signaling object",
                        ));
                    }
                }
            }
        }
        Ok(Self { sections })
    }
}

fn valid_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_-./".contains(&byte))
}

impl Section {
    fn position(&self, key: &str) -> usize {
        self.options.get(key).map_or(self.line, |(line, _)| *line)
    }

    fn value(&self, key: &str) -> Option<&str> {
        self.options.get(key).map(|(_, value)| value.as_str())
    }

    fn required(&self, file: &str, key: &str) -> Result<&str, ConfigError> {
        self.value(key)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| ConfigError::at(file, self.line, format!("missing {key}")))
    }

    fn check(&self, file: &str, keys: &[&str]) -> Result<(), ConfigError> {
        for (key, (line, _)) in &self.options {
            if !keys.contains(&key.as_str()) {
                return Err(ConfigError::at(
                    file,
                    *line,
                    format!("unsupported option {key}"),
                ));
            }
        }
        Ok(())
    }

    fn flag(&self, file: &str, key: &str, default: bool) -> Result<bool, ConfigError> {
        match self.value(key) {
            None => Ok(default),
            Some("yes" | "true" | "1") => Ok(true),
            Some("no" | "false" | "0") => Ok(false),
            Some(_) => Err(ConfigError::at(
                file,
                self.position(key),
                format!("{key} must be yes or no"),
            )),
        }
    }

    fn number<T: std::str::FromStr>(
        &self,
        file: &str,
        key: &str,
        default: T,
    ) -> Result<T, ConfigError> {
        match self.value(key) {
            None => Ok(default),
            Some(value) => value
                .parse()
                .map_err(|_| ConfigError::at(file, self.position(key), format!("invalid {key}"))),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TlsFiles {
    pub certificate: String,
    pub private_key: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Http {
    pub bind: SocketAddr,
    pub tls: Option<TlsFiles>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AriUser {
    pub name: String,
    pub password: Secret,
    pub read_only: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Protocol {
    Udp,
    Tcp,
    Tls,
    Ws,
    Wss,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Transport {
    pub name: String,
    pub protocol: Protocol,
    pub bind: SocketAddr,
    pub external_media_address: Option<IpAddr>,
    pub tls: Option<TlsFiles>,
    pub ca_list_file: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Auth {
    pub name: String,
    pub username: String,
    pub password: Secret,
    pub realm: String,
}

impl Auth {
    pub fn inbound_realm(&self) -> &str {
        if self.realm.is_empty() {
            "asterisk"
        } else {
            &self.realm
        }
    }

    pub fn matches_outbound_realm(&self, realm: &str) -> bool {
        self.realm.is_empty() || self.realm == "*" || self.realm == realm
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Aor {
    pub name: String,
    pub max_contacts: u16,
    pub contact: Option<String>,
    pub minimum_expiration: u32,
    pub default_expiration: u32,
    pub maximum_expiration: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    pub name: String,
    pub transport: String,
    pub auth: String,
    pub outbound_auth: Vec<String>,
    pub aors: Vec<String>,
    pub context: String,
    pub codecs: Vec<Codec>,
    pub webrtc: bool,
    pub dtmf: Dtmf,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    Pcmu,
    Pcma,
    Opus,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dtmf {
    Rfc4733,
    Info,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Registration {
    pub name: String,
    pub transport: String,
    pub outbound_auth: String,
    pub server_uri: RegistrarUri,
    pub client_uri: RegistrationAddress,
    pub expiration: u32,
    pub contact_user: String,
    pub retry_interval: u32,
    pub forbidden_retry_interval: u32,
    pub fatal_retry_interval: u32,
    pub max_retries: u32,
    pub max_random_initial_delay: u32,
    pub auth_rejection_permanent: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identify {
    pub endpoint: String,
    pub address: IpAddr,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rtp {
    pub start: u16,
    pub end: u16,
    pub stun: Option<String>,
    pub turn: Option<String>,
    pub turn_username: Option<String>,
    pub turn_password: Option<Secret>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub websocket: WebSocketConfig,
    pub http: Http,
    pub users: Vec<AriUser>,
    pub origins: Vec<String>,
    pub transports: Vec<Transport>,
    pub auth: Vec<Auth>,
    pub aors: Vec<Aor>,
    pub endpoints: Vec<Endpoint>,
    pub registrations: Vec<Registration>,
    pub identify: Vec<Identify>,
    pub rtp: Rtp,
    pub dialplan: Dialplan,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reload {
    Apply,
    RestartRequired,
}

impl Config {
    pub fn load(directory: &Path) -> Result<Self, ConfigError> {
        enum Missing {
            Reject,
            Empty,
        }
        fn read(directory: &Path, file: &str, missing: Missing) -> Result<String, ConfigError> {
            use std::io::Read;
            let input = match std::fs::File::open(directory.join(file)) {
                Ok(input) => input,
                Err(error)
                    if error.kind() == std::io::ErrorKind::NotFound
                        && matches!(missing, Missing::Empty) =>
                {
                    return Ok(String::new());
                }
                Err(error) => return Err(ConfigError::at(file, 0, error.to_string())),
            };
            let mut source = String::new();
            input
                .take(MAX_FILE as u64 + 1)
                .read_to_string(&mut source)
                .map_err(|error| ConfigError::at(file, 0, error.to_string()))?;
            if source.len() > MAX_FILE {
                return Err(ConfigError::at(file, 0, "file exceeds 1 MiB"));
            }
            Ok(source)
        }
        Self::parse(
            &read(directory, "http.conf", Missing::Reject)?,
            &read(directory, "ari.conf", Missing::Reject)?,
            &read(directory, "pjsip.conf", Missing::Reject)?,
            &read(directory, "rtp.conf", Missing::Reject)?,
            &read(directory, "extensions.conf", Missing::Reject)?,
        )?
        .with_websocket_configuration(
            &read(directory, "chan_websocket.conf", Missing::Empty)?,
            &read(directory, "websocket_client.conf", Missing::Empty)?,
        )
    }

    pub fn parse(
        http: &str,
        ari: &str,
        sip: &str,
        rtp: &str,
        dialplan: &str,
    ) -> Result<Self, ConfigError> {
        let http_ini = Ini::parse("http.conf", http)?;
        if http_ini.sections.len() != 1 {
            return Err(ConfigError::at(
                "http.conf",
                1,
                "one general section is required",
            ));
        }
        let general = &http_ini.sections[0];
        if general.name != "general" {
            return Err(ConfigError::at(
                "http.conf",
                general.line,
                "expected general section",
            ));
        }
        general.check(
            "http.conf",
            &[
                "enabled",
                "bindaddr",
                "bindport",
                "tlsenable",
                "tlsbindaddr",
                "tlscertfile",
                "tlsprivatekey",
            ],
        )?;
        if !general.flag("http.conf", "enabled", true)? {
            return Err(ConfigError::at(
                "http.conf",
                general.line,
                "disabled management service is unsupported",
            ));
        }
        let ip: IpAddr = general
            .value("bindaddr")
            .unwrap_or("127.0.0.1")
            .parse()
            .map_err(|_| ConfigError::at("http.conf", general.line, "invalid bindaddr"))?;
        let tls = if general.flag("http.conf", "tlsenable", false)? {
            Some(TlsFiles {
                certificate: general.required("http.conf", "tlscertfile")?.into(),
                private_key: general.required("http.conf", "tlsprivatekey")?.into(),
            })
        } else {
            None
        };
        let bind = if tls.is_some() {
            general
                .value("tlsbindaddr")
                .unwrap_or("127.0.0.1:8089")
                .parse()
                .map_err(|_| ConfigError::at("http.conf", general.line, "invalid tlsbindaddr"))?
        } else {
            SocketAddr::new(ip, general.number("http.conf", "bindport", 8088)?)
        };
        if bind.port() == 0 || (!bind.ip().is_loopback() && tls.is_none()) {
            return Err(ConfigError::at(
                "http.conf",
                general.line,
                "public management requires TLS and a nonzero port",
            ));
        }
        let mut config = Self {
            websocket: WebSocketConfig::default(),
            http: Http { bind, tls },
            users: Vec::new(),
            origins: Vec::new(),
            transports: Vec::new(),
            auth: Vec::new(),
            aors: Vec::new(),
            endpoints: Vec::new(),
            registrations: Vec::new(),
            identify: Vec::new(),
            rtp: Rtp {
                start: 10000,
                end: 20000,
                stun: None,
                turn: None,
                turn_username: None,
                turn_password: None,
            },
            dialplan: Dialplan::parse(dialplan)?,
        };
        for section in Ini::parse("ari.conf", ari)?.sections {
            if section.name == "general" {
                section.check("ari.conf", &["enabled", "allowed_origins"])?;
                if !section.flag("ari.conf", "enabled", true)? {
                    return Err(ConfigError::at(
                        "ari.conf",
                        section.line,
                        "ARI must be enabled",
                    ));
                }
                for origin in section
                    .value("allowed_origins")
                    .unwrap_or("")
                    .split(',')
                    .filter(|value| !value.is_empty())
                {
                    let origin = origin.trim();
                    if !valid_origin(origin) {
                        return Err(ConfigError::at(
                            "ari.conf",
                            section.position("allowed_origins"),
                            "allowed_origins requires exact secure or loopback origins",
                        ));
                    }
                    config.origins.push(origin.into());
                }
            } else {
                section.check("ari.conf", &["type", "password", "read_only"])?;
                if section.required("ari.conf", "type")? != "user" {
                    return Err(ConfigError::at(
                        "ari.conf",
                        section.line,
                        "expected type=user",
                    ));
                }
                config.users.push(AriUser {
                    name: section.name.clone(),
                    password: Secret(section.required("ari.conf", "password")?.into()),
                    read_only: section.flag("ari.conf", "read_only", false)?,
                });
            }
        }
        if config.users.is_empty() {
            return Err(ConfigError::at(
                "ari.conf",
                1,
                "at least one user is required",
            ));
        }
        let signaling = Ini::parse_signaling("pjsip.conf", sip)?;
        for section in &signaling.sections {
            if section.value("type") != Some("registration") {
                config.sip_section(section)?;
            }
        }
        for section in &signaling.sections {
            if section.value("type") == Some("registration") {
                config.sip_section(section)?;
            }
        }
        config.validate_references()?;
        for section in Ini::parse("rtp.conf", rtp)?.sections {
            if section.name != "general" {
                return Err(ConfigError::at(
                    "rtp.conf",
                    section.line,
                    "expected general section",
                ));
            }
            section.check(
                "rtp.conf",
                &[
                    "rtpstart",
                    "rtpend",
                    "stunaddr",
                    "turnaddr",
                    "turnusername",
                    "turnpassword",
                ],
            )?;
            config.rtp = Rtp {
                start: section.number("rtp.conf", "rtpstart", 10000)?,
                end: section.number("rtp.conf", "rtpend", 20000)?,
                stun: section.value("stunaddr").map(String::from),
                turn: section.value("turnaddr").map(String::from),
                turn_username: section.value("turnusername").map(String::from),
                turn_password: section
                    .value("turnpassword")
                    .map(|value| Secret(value.into())),
            };
        }
        if config.rtp.start < 1024
            || !config.rtp.start.is_multiple_of(2)
            || config.rtp.end <= config.rtp.start
        {
            return Err(ConfigError::at(
                "rtp.conf",
                1,
                "RTP range must start at an even unprivileged port and include RTCP",
            ));
        }
        Ok(config)
    }

    fn sip_section(&mut self, section: &Section) -> Result<(), ConfigError> {
        let file = "pjsip.conf";
        match section.required(file, "type")? {
            "transport" => {
                if self.transports.len() == MAX_TRANSPORTS {
                    return Err(ConfigError::at(
                        file,
                        section.line,
                        "too many signaling transports",
                    ));
                }
                section.check(
                    file,
                    &[
                        "type",
                        "protocol",
                        "bind",
                        "external_media_address",
                        "cert_file",
                        "priv_key_file",
                        "ca_list_file",
                    ],
                )?;
                let protocol = match section.required(file, "protocol")? {
                    "udp" => Protocol::Udp,
                    "tcp" => Protocol::Tcp,
                    "tls" => Protocol::Tls,
                    "ws" => Protocol::Ws,
                    "wss" => Protocol::Wss,
                    _ => {
                        return Err(ConfigError::at(
                            file,
                            section.line,
                            "unsupported transport protocol",
                        ));
                    }
                };
                let websocket = matches!(protocol, Protocol::Ws | Protocol::Wss);
                if websocket {
                    if matches!(protocol, Protocol::Wss) != self.http.tls.is_some() {
                        return Err(ConfigError::at(
                            file,
                            section.position("protocol"),
                            "WebSocket transport does not match the configured HTTP listener scheme",
                        ));
                    }
                    if self
                        .transports
                        .iter()
                        .any(|transport| transport.protocol == protocol)
                    {
                        return Err(ConfigError::at(
                            file,
                            section.position("protocol"),
                            "WebSocket transport metadata is ambiguous for the HTTP listener",
                        ));
                    }
                }
                let tls = if protocol == Protocol::Tls {
                    Some(TlsFiles {
                        certificate: section.required(file, "cert_file")?.into(),
                        private_key: section.required(file, "priv_key_file")?.into(),
                    })
                } else {
                    for key in ["cert_file", "priv_key_file", "ca_list_file"] {
                        if section.value(key).is_some() {
                            let reason = if websocket {
                                "WebSocket encryption must be configured in http.conf".into()
                            } else {
                                format!("{key} requires an encrypted transport")
                            };
                            return Err(ConfigError::at(file, section.position(key), reason));
                        }
                    }
                    None
                };
                let ca_list_file = section.value("ca_list_file").map(str::to_owned);
                if ca_list_file.as_ref().is_some_and(String::is_empty) {
                    return Err(ConfigError::at(
                        file,
                        section.position("ca_list_file"),
                        "empty certificate authority list path",
                    ));
                }
                let configured_bind = section.required(file, "bind")?;
                let bind = if websocket {
                    if configured_bind.parse::<SocketAddr>().is_err()
                        && configured_bind.parse::<IpAddr>().is_err()
                    {
                        return Err(ConfigError::at(
                            file,
                            section.position("bind"),
                            "invalid transport bind",
                        ));
                    }
                    self.http.bind
                } else {
                    configured_bind.parse().map_err(|_| {
                        ConfigError::at(file, section.position("bind"), "invalid transport bind")
                    })?
                };
                let external_media_address = section
                    .value("external_media_address")
                    .map(|value| {
                        value.parse().map_err(|_| {
                            ConfigError::at(file, section.line, "invalid external media address")
                        })
                    })
                    .transpose()?;
                self.transports.push(Transport {
                    name: section.name.clone(),
                    protocol,
                    bind,
                    external_media_address,
                    tls,
                    ca_list_file,
                });
            }
            "auth" => {
                section.check(
                    file,
                    &["type", "auth_type", "username", "password", "realm"],
                )?;
                if section.value("auth_type").unwrap_or("userpass") != "userpass" {
                    return Err(ConfigError::at(
                        file,
                        section.line,
                        "only userpass authentication is supported",
                    ));
                }
                self.auth.push(Auth {
                    name: section.name.clone(),
                    username: section.required(file, "username")?.into(),
                    password: Secret(section.required(file, "password")?.into()),
                    realm: section.value("realm").unwrap_or("").into(),
                });
            }
            "aor" => {
                section.check(
                    file,
                    &[
                        "type",
                        "max_contacts",
                        "contact",
                        "minimum_expiration",
                        "default_expiration",
                        "maximum_expiration",
                    ],
                )?;
                let max_contacts = section.number(file, "max_contacts", 0)?;
                if max_contacts > 16 {
                    return Err(ConfigError::at(
                        file,
                        section.position("max_contacts"),
                        "max_contacts must be 0 through 16",
                    ));
                }
                let minimum_expiration = section.number(file, "minimum_expiration", 60)?;
                let maximum_expiration = section.number(file, "maximum_expiration", 7200)?;
                if minimum_expiration > maximum_expiration {
                    return Err(ConfigError::at(
                        file,
                        section.position("minimum_expiration"),
                        "minimum_expiration must not exceed maximum_expiration",
                    ));
                }
                self.aors.push(Aor {
                    name: section.name.clone(),
                    max_contacts,
                    contact: section.value("contact").map(String::from),
                    minimum_expiration,
                    default_expiration: section.number(file, "default_expiration", 3600)?,
                    maximum_expiration,
                });
            }
            "endpoint" => {
                section.check(
                    file,
                    &[
                        "type",
                        "transport",
                        "auth",
                        "outbound_auth",
                        "aors",
                        "context",
                        "disallow",
                        "allow",
                        "webrtc",
                        "dtmf_mode",
                    ],
                )?;
                if section.value("disallow").unwrap_or("all") != "all" {
                    return Err(ConfigError::at(file, section.line, "disallow must be all"));
                }
                let mut codecs = Vec::new();
                for codec in section.value("allow").unwrap_or("ulaw,alaw").split(',') {
                    codecs.push(match codec.trim() {
                        "ulaw" => Codec::Pcmu,
                        "alaw" => Codec::Pcma,
                        "opus" => Codec::Opus,
                        _ => return Err(ConfigError::at(file, section.line, "unsupported codec")),
                    });
                }
                let dtmf = match section.value("dtmf_mode").unwrap_or("rfc4733") {
                    "rfc4733" => Dtmf::Rfc4733,
                    "info" => Dtmf::Info,
                    _ => return Err(ConfigError::at(file, section.line, "unsupported dtmf_mode")),
                };
                self.endpoints.push(Endpoint {
                    name: section.name.clone(),
                    transport: section.required(file, "transport")?.into(),
                    auth: section.required(file, "auth")?.into(),
                    outbound_auth: section
                        .value("outbound_auth")
                        .unwrap_or("")
                        .split(',')
                        .filter(|value| !value.trim().is_empty())
                        .map(|value| value.trim().into())
                        .collect(),
                    aors: section
                        .required(file, "aors")?
                        .split(',')
                        .map(|value| value.trim().into())
                        .collect(),
                    context: section.value("context").unwrap_or("default").into(),
                    codecs,
                    webrtc: section.flag(file, "webrtc", false)?,
                    dtmf,
                });
            }
            "registration" => {
                section.check(
                    file,
                    &[
                        "type",
                        "transport",
                        "outbound_auth",
                        "server_uri",
                        "client_uri",
                        "expiration",
                        "contact_user",
                        "retry_interval",
                        "forbidden_retry_interval",
                        "fatal_retry_interval",
                        "max_retries",
                        "max_random_initial_delay",
                        "auth_rejection_permanent",
                    ],
                )?;
                if self.registrations.len() >= MAX_REGISTRATIONS {
                    return Err(ConfigError::at(
                        file,
                        section.line,
                        "registration client limit is 16",
                    ));
                }
                self.registrations.push(registration::parse(
                    file,
                    section,
                    &self.transports,
                    &self.auth,
                )?);
            }
            "identify" => {
                section.check(file, &["type", "endpoint", "match"])?;
                self.identify.push(Identify {
                    endpoint: section.required(file, "endpoint")?.into(),
                    address: section.required(file, "match")?.parse().map_err(|_| {
                        ConfigError::at(file, section.line, "identify requires an IP address")
                    })?,
                });
            }
            _ => {
                return Err(ConfigError::at(
                    file,
                    section.line,
                    "unsupported object type",
                ));
            }
        }
        Ok(())
    }

    fn validate_references(&self) -> Result<(), ConfigError> {
        for endpoint in &self.endpoints {
            if !self
                .transports
                .iter()
                .any(|value| value.name == endpoint.transport)
                || !self.auth.iter().any(|value| value.name == endpoint.auth)
                || endpoint.outbound_auth.len() > 8
                || endpoint
                    .outbound_auth
                    .iter()
                    .any(|name| !self.auth.iter().any(|value| &value.name == name))
                || endpoint
                    .aors
                    .iter()
                    .any(|name| !self.aors.iter().any(|value| &value.name == name))
            {
                return Err(ConfigError::at(
                    "pjsip.conf",
                    1,
                    format!("unresolved reference in endpoint {}", endpoint.name),
                ));
            }
        }
        for registration in &self.registrations {
            if !self
                .transports
                .iter()
                .any(|value| value.name == registration.transport)
                || !self
                    .auth
                    .iter()
                    .any(|value| value.name == registration.outbound_auth)
            {
                return Err(ConfigError::at(
                    "pjsip.conf",
                    1,
                    "unresolved registration reference",
                ));
            }
        }
        for identify in &self.identify {
            if !self
                .endpoints
                .iter()
                .any(|value| value.name == identify.endpoint)
            {
                return Err(ConfigError::at(
                    "pjsip.conf",
                    1,
                    "unresolved identify endpoint",
                ));
            }
        }
        Ok(())
    }

    pub fn reload(&self, replacement: &Self) -> Reload {
        if self.http != replacement.http
            || self.transports != replacement.transports
            || self.rtp != replacement.rtp
            || (self.origins != replacement.origins
                && self
                    .transports
                    .iter()
                    .any(|transport| matches!(transport.protocol, Protocol::Ws | Protocol::Wss)))
        {
            Reload::RestartRequired
        } else {
            Reload::Apply
        }
    }
}

fn valid_origin(origin: &str) -> bool {
    if origin.len() > 256 || origin.contains('*') {
        return false;
    }
    let Ok(url) = url::Url::parse(origin) else {
        return false;
    };
    let loopback = match url.host() {
        Some(url::Host::Domain("localhost")) => true,
        Some(url::Host::Ipv4(address)) => address.is_loopback(),
        Some(url::Host::Ipv6(address)) => address.is_loopback(),
        Some(url::Host::Domain(_)) | None => false,
    };
    (url.scheme() == "https" || (url.scheme() == "http" && loopback))
        && url.username().is_empty()
        && url.password().is_none()
        && url.path() == "/"
        && url.query().is_none()
        && url.fragment().is_none()
        && url.origin().ascii_serialization() == origin
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dialplan {
    pub steps: Vec<Step>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step {
    pub context: String,
    pub extension: String,
    pub priority: u16,
    pub application: Application,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Application {
    NoOp(String),
    Set {
        variable: String,
        value: String,
    },
    Answer,
    Dial {
        destinations: Vec<String>,
        timeout: Option<u16>,
    },
    Stasis {
        application: String,
        arguments: Vec<String>,
    },
    Playback(String),
    Wait(WaitDuration),
    Goto {
        context: Option<String>,
        extension: Option<String>,
        priority: u16,
    },
    Hangup,
}

impl Dialplan {
    pub fn parse(source: &str) -> Result<Self, ConfigError> {
        let file = "extensions.conf";
        if source.len() > MAX_FILE {
            return Err(ConfigError::at(file, 1, "file exceeds 1 MiB"));
        }
        let mut context = String::new();
        let mut extension = String::new();
        let mut priority = 0u16;
        let mut steps = Vec::new();
        for (index, raw) in source.lines().enumerate() {
            let line = raw.split(';').next().unwrap_or("").trim();
            let error = |reason: &str| ConfigError::at(file, index + 1, reason);
            if line.is_empty() {
                continue;
            }
            if line.len() > 4096 || steps.len() >= 4096 {
                return Err(error("dialplan capacity exceeded"));
            }
            if let Some(name) = line
                .strip_prefix('[')
                .and_then(|value| value.strip_suffix(']'))
            {
                if !valid_name(name) {
                    return Err(error("invalid context"));
                }
                context = name.into();
                extension.clear();
                priority = 0;
                continue;
            }
            if context.is_empty() {
                return Err(error("instruction outside a context"));
            }
            let (key, value) = line
                .split_once("=>")
                .ok_or_else(|| error("expected exten => or same =>"))?;
            let value = match key.trim() {
                "exten" => {
                    let (name, tail) = value
                        .split_once(',')
                        .ok_or_else(|| error("missing extension priority"))?;
                    extension = name.trim().into();
                    priority = 0;
                    tail
                }
                "same" if !extension.is_empty() => value,
                _ => return Err(error("unsupported dialplan directive")),
            };
            let (rank, invocation) = value
                .split_once(',')
                .ok_or_else(|| error("missing application"))?;
            priority = if rank.trim() == "n" {
                priority
                    .checked_add(1)
                    .ok_or_else(|| error("priority overflow"))?
            } else {
                rank.trim().parse().map_err(|_| error("invalid priority"))?
            };
            if priority == 0 {
                return Err(error("priority starts at 1"));
            }
            let invocation = invocation.trim();
            let (name, arguments) = invocation
                .split_once('(')
                .ok_or_else(|| error("application parentheses are required"))?;
            let arguments = arguments
                .strip_suffix(')')
                .ok_or_else(|| error("unclosed application"))?;
            validate_expansion(arguments).map_err(error)?;
            let application = match name {
                "NoOp" => Application::NoOp(arguments.into()),
                "Set" => {
                    let (variable, value) = arguments
                        .split_once('=')
                        .ok_or_else(|| error("Set requires a variable and value"))?;
                    if !valid_variable_expression(variable) {
                        return Err(error("invalid Set variable"));
                    }
                    Application::Set {
                        variable: variable.into(),
                        value: value.into(),
                    }
                }
                "Answer" if arguments.is_empty() => Application::Answer,
                "Hangup" if arguments.is_empty() => Application::Hangup,
                "Playback" if valid_name(arguments) => Application::Playback(arguments.into()),
                "Wait" => Application::Wait(
                    WaitDuration::parse(arguments).map_err(|reason| error(&reason.to_string()))?,
                ),
                "Stasis" => {
                    let mut parts = arguments.split(',');
                    let name = parts.next().unwrap_or("");
                    if !valid_name(name) {
                        return Err(error("invalid Stasis application"));
                    }
                    Application::Stasis {
                        application: name.into(),
                        arguments: parts.map(String::from).collect(),
                    }
                }
                "Dial" => {
                    let mut parts = arguments.split(',');
                    let destinations: Vec<String> = parts
                        .next()
                        .unwrap_or("")
                        .split('&')
                        .map(String::from)
                        .collect();
                    if destinations.is_empty()
                        || destinations.len() > 16
                        || destinations
                            .iter()
                            .any(|value| !value.starts_with("PJSIP/") || value.len() <= 6)
                    {
                        return Err(error("Dial requires PJSIP destinations"));
                    }
                    let timeout = parts
                        .next()
                        .filter(|value| !value.is_empty())
                        .map(str::parse)
                        .transpose()
                        .map_err(|_| error("invalid Dial timeout"))?;
                    if timeout == Some(0) || parts.next().is_some() {
                        return Err(error("unsupported Dial arguments"));
                    }
                    Application::Dial {
                        destinations,
                        timeout,
                    }
                }
                "Goto" => {
                    let parts: Vec<&str> = arguments.split(',').collect();
                    let (context, extension, rank) = match parts.as_slice() {
                        [rank] => (None, None, *rank),
                        [extension, rank] => (None, Some((*extension).into()), *rank),
                        [context, extension, rank] => {
                            (Some((*context).into()), Some((*extension).into()), *rank)
                        }
                        _ => return Err(error("invalid Goto arguments")),
                    };
                    Application::Goto {
                        context,
                        extension,
                        priority: rank
                            .parse()
                            .map_err(|_| error("Goto requires a numeric priority"))?,
                    }
                }
                _ => return Err(error("unsupported application or arguments")),
            };
            if steps.iter().any(|step: &Step| {
                step.context == context && step.extension == extension && step.priority == priority
            }) {
                return Err(error("duplicate dialplan priority"));
            }
            steps.push(Step {
                context: context.clone(),
                extension: extension.clone(),
                priority,
                application,
            });
        }
        Ok(Self { steps })
    }
}

fn valid_variable_expression(name: &str) -> bool {
    valid_name(name) || matches!(name, "CALLERID(num)" | "CALLERID(name)" | "CALLERID(all)")
}

fn validate_expansion(mut value: &str) -> Result<(), &'static str> {
    while let Some((_, tail)) = value.split_once("${") {
        let (name, rest) = tail.split_once('}').ok_or("unclosed variable expansion")?;
        if !valid_variable_expression(name) {
            return Err("unsupported variable expression");
        }
        value = rest;
    }
    Ok(())
}
