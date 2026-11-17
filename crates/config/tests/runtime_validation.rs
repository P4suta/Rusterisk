use rusterisk_config::{
    Application, Config, ConfigError, Dialplan, Protocol, TlsFiles, MAX_REGISTRATIONS,
    MAX_TRANSPORTS,
};

const HTTP: &str = "[general]\nbindaddr=127.0.0.1\nbindport=8088\n";
const ARI: &str = "[app]\ntype=user\npassword=test-password\n";
const SIP: &str = "[udp]\ntype=transport\nprotocol=udp\nbind=127.0.0.1:5060\n\
[browser]\ntype=transport\nprotocol=ws\nbind=127.0.0.1\n\
[credential]\ntype=auth\nusername=phone\npassword=test-password\n\
[phone-aor]\ntype=aor\nmax_contacts=1\n\
[phone]\ntype=endpoint\ntransport=udp\nauth=credential\naors=phone-aor\ncontext=internal\nallow=ulaw\n\
[provider]\ntype=registration\ntransport=udp\noutbound_auth=credential\nserver_uri=sip:provider.example\nclient_uri=sip:phone@provider.example\n";
const CLIENT: &str = "[media]\ntype=websocket_client\nconnection_type=per_call_config\nuri=ws://127.0.0.1/media\nprotocols=media\n";

fn configuration() -> Result<Config, ConfigError> {
    Config::parse(
        HTTP,
        ARI,
        SIP,
        "",
        "[internal]\nexten => 100,1,NoOp(start)\n",
    )?
    .with_websocket_configuration("", CLIENT)
}

fn missing(reason: impl Into<String>) -> ConfigError {
    ConfigError {
        file: "runtime_validation.rs".into(),
        line: 0,
        reason: reason.into(),
    }
}

fn first<T>(values: &mut [T]) -> Result<&mut T, ConfigError> {
    values
        .first_mut()
        .ok_or_else(|| missing("fixture object is missing"))
}

#[derive(Clone, Copy, Debug)]
enum Mutation {
    MissingUsers,
    WildcardOrigin,
    EmptyCodecs,
    InvertedAorExpiry,
    UnsupportedContact,
    InvalidWebSocketAttempts,
    ZeroDialplanPriority,
    TransportCapacity,
    ObjectNameCapacity,
    MissingReference,
}

impl Mutation {
    const ALL: [Self; 10] = [
        Self::MissingUsers,
        Self::WildcardOrigin,
        Self::EmptyCodecs,
        Self::InvertedAorExpiry,
        Self::UnsupportedContact,
        Self::InvalidWebSocketAttempts,
        Self::ZeroDialplanPriority,
        Self::TransportCapacity,
        Self::ObjectNameCapacity,
        Self::MissingReference,
    ];

    fn apply(self, config: &mut Config) -> Result<(), ConfigError> {
        match self {
            Self::MissingUsers => config.users.clear(),
            Self::WildcardOrigin => config.origins.push("*".into()),
            Self::EmptyCodecs => first(&mut config.endpoints)?.codecs.clear(),
            Self::InvertedAorExpiry => {
                let aor = first(&mut config.aors)?;
                aor.minimum_expiration = 61;
                aor.maximum_expiration = 60;
            }
            Self::UnsupportedContact => {
                first(&mut config.aors)?.contact = Some("sip:127.0.0.1;transport=tls".into());
            }
            Self::InvalidWebSocketAttempts => {
                first(&mut config.websocket.clients)?.reconnect_attempts = 0;
            }
            Self::ZeroDialplanPriority => first(&mut config.dialplan.steps)?.priority = 0,
            Self::TransportCapacity => {
                let prototype = first(&mut config.transports)?.clone();
                for index in config.transports.len()..=MAX_TRANSPORTS {
                    let mut transport = prototype.clone();
                    transport.name = format!("extra-{index}");
                    let port =
                        u16::try_from(10000 + index).map_err(|error| missing(error.to_string()))?;
                    transport.bind.set_port(port);
                    config.transports.push(transport);
                }
            }
            Self::ObjectNameCapacity => first(&mut config.endpoints)?.name = "x".repeat(129),
            Self::MissingReference => first(&mut config.endpoints)?.transport = "absent".into(),
        }
        Ok(())
    }

    const fn file(self) -> &'static str {
        match self {
            Self::MissingUsers | Self::WildcardOrigin => "ari.conf",
            Self::InvalidWebSocketAttempts => "websocket_client.conf",
            Self::ZeroDialplanPriority => "extensions.conf",
            Self::EmptyCodecs
            | Self::InvertedAorExpiry
            | Self::UnsupportedContact
            | Self::TransportCapacity
            | Self::ObjectNameCapacity
            | Self::MissingReference => "pjsip.conf",
        }
    }
}

fn refused(config: &Config, file: &str) -> Result<ConfigError, ConfigError> {
    let result: Result<(), ConfigError> = config.validate_runtime();
    let error = result
        .err()
        .ok_or_else(|| missing("invalid runtime settings were accepted"))?;
    assert_eq!(error.file, file);
    assert!(!error.reason.is_empty());
    Ok(error)
}

#[test]
fn runtime_validation_rejects_mutated_public_values_capacity_and_references(
) -> Result<(), ConfigError> {
    configuration()?.validate_runtime()?;
    for mutation in Mutation::ALL {
        let mut candidate = configuration()?;
        mutation.apply(&mut candidate)?;
        refused(&candidate, mutation.file())
            .map_err(|error| missing(format!("{mutation:?}: {error}")))?;
    }
    Ok(())
}

#[test]
fn runtime_validation_preserves_existing_unsigned_and_optional_setting_contracts(
) -> Result<(), ConfigError> {
    for (seconds, context) in [(0, ""), (u32::MAX, "unlisted context : literal")] {
        let mut candidate = configuration()?;
        first(&mut candidate.aors)?.default_expiration = seconds;
        first(&mut candidate.endpoints)?.context = context.into();
        let registration = first(&mut candidate.registrations)?;
        registration.expiration = seconds;
        registration.retry_interval = seconds;
        registration.forbidden_retry_interval = seconds;
        registration.fatal_retry_interval = seconds;
        registration.max_retries = seconds;
        registration.max_random_initial_delay = seconds;
        candidate.validate_runtime()?;
    }
    let empty = Config::parse(HTTP, ARI, "", "", "")?;
    assert!(empty.transports.is_empty());
    assert!(empty.dialplan.steps.is_empty());
    empty.validate_runtime()
}

#[test]
fn runtime_validation_preserves_requested_ephemeral_listeners_and_media_ports(
) -> Result<(), ConfigError> {
    for media in [(10000, 20000), (0, 0)] {
        let mut candidate = configuration()?;
        candidate.http.bind.set_port(0);
        let websocket = candidate
            .transports
            .iter()
            .find(|transport| transport.protocol == Protocol::Ws)
            .ok_or_else(|| missing("fixture WebSocket transport is missing"))?;
        assert_eq!(websocket.bind.port(), 8088);
        first(&mut candidate.transports)?.bind.set_port(0);
        candidate.rtp.start = media.0;
        candidate.rtp.end = media.1;
        candidate.validate_runtime()?;
    }
    Ok(())
}

#[test]
fn runtime_ephemeral_media_requires_both_zeros_and_persistent_parsing_keeps_its_rules(
) -> Result<(), ConfigError> {
    for (start, end) in [(0, 10001), (10000, 0), (10001, 10002), (10000, 10000)] {
        let mut candidate = configuration()?;
        candidate.rtp.start = start;
        candidate.rtp.end = end;
        refused(&candidate, "rtp.conf")?;
    }
    assert!(Config::parse(
        "[general]\nbindaddr=127.0.0.1\nbindport=0\n",
        ARI,
        "",
        "",
        "",
    )
    .is_err());
    assert!(Config::parse(HTTP, ARI, "", "[general]\nrtpstart=0\nrtpend=0\n", "",).is_err());
    Ok(())
}

#[derive(Clone, Copy, Debug)]
enum AccessMutation {
    PublicPlaintext,
    UnencryptedTlsMaterial,
    MissingTlsMaterial,
    DuplicateUser,
    DuplicateEndpoint,
    AorContactCapacity,
    EmptyAorReferences,
    OutboundAuthCapacity,
}

impl AccessMutation {
    const ALL: [Self; 8] = [
        Self::PublicPlaintext,
        Self::UnencryptedTlsMaterial,
        Self::MissingTlsMaterial,
        Self::DuplicateUser,
        Self::DuplicateEndpoint,
        Self::AorContactCapacity,
        Self::EmptyAorReferences,
        Self::OutboundAuthCapacity,
    ];

    fn apply(self, config: &mut Config) -> Result<(), ConfigError> {
        match self {
            Self::PublicPlaintext => config.http.bind.set_ip([0, 0, 0, 0].into()),
            Self::UnencryptedTlsMaterial => {
                first(&mut config.transports)?.tls = Some(TlsFiles {
                    certificate: "certificate.pem".into(),
                    private_key: "key.pem".into(),
                });
            }
            Self::MissingTlsMaterial => {
                let mut transport = secure_transport()?;
                transport.tls = None;
                config.transports.push(transport);
            }
            Self::DuplicateUser => {
                let user = first(&mut config.users)?.clone();
                config.users.push(user);
            }
            Self::DuplicateEndpoint => {
                let endpoint = first(&mut config.endpoints)?.clone();
                config.endpoints.push(endpoint);
            }
            Self::AorContactCapacity => first(&mut config.aors)?.max_contacts = 17,
            Self::EmptyAorReferences => first(&mut config.endpoints)?.aors.clear(),
            Self::OutboundAuthCapacity => {
                first(&mut config.endpoints)?.outbound_auth =
                    std::iter::repeat_n("credential".into(), 9).collect();
            }
        }
        Ok(())
    }

    const fn file(self) -> &'static str {
        match self {
            Self::PublicPlaintext => "http.conf",
            Self::DuplicateUser => "ari.conf",
            Self::UnencryptedTlsMaterial
            | Self::MissingTlsMaterial
            | Self::DuplicateEndpoint
            | Self::AorContactCapacity
            | Self::EmptyAorReferences
            | Self::OutboundAuthCapacity => "pjsip.conf",
        }
    }
}

fn secure_transport() -> Result<rusterisk_config::Transport, ConfigError> {
    Config::parse(
        HTTP,
        ARI,
        "[secure]\ntype=transport\nprotocol=tls\nbind=127.0.0.1:5061\ncert_file=certificate.pem\npriv_key_file=key.pem\n",
        "",
        "",
    )?
    .transports
    .pop()
    .ok_or_else(|| missing("fixture TLS transport is missing"))
}

#[test]
fn runtime_validation_checks_access_tls_and_object_shapes_before_io() -> Result<(), ConfigError> {
    for mutation in AccessMutation::ALL {
        let mut candidate = configuration()?;
        mutation.apply(&mut candidate)?;
        refused(&candidate, mutation.file())
            .map_err(|error| missing(format!("{mutation:?}: {error}")))?;
    }
    Ok(())
}

#[derive(Clone, Copy, Debug)]
enum ProfileMutation {
    RegistrationCapacity,
    RegistrationProtocol,
    RegistrationContactUser,
    WebSocketCredentialPair,
    WebSocketBudget,
    WebSocketPlaintextCa,
    WebSocketUri,
    WebSocketCapacity,
}

impl ProfileMutation {
    const ALL: [Self; 8] = [
        Self::RegistrationCapacity,
        Self::RegistrationProtocol,
        Self::RegistrationContactUser,
        Self::WebSocketCredentialPair,
        Self::WebSocketBudget,
        Self::WebSocketPlaintextCa,
        Self::WebSocketUri,
        Self::WebSocketCapacity,
    ];

    fn apply(self, config: &mut Config) -> Result<(), ConfigError> {
        match self {
            Self::RegistrationCapacity => {
                let prototype = first(&mut config.registrations)?.clone();
                for index in config.registrations.len()..=MAX_REGISTRATIONS {
                    let mut registration = prototype.clone();
                    registration.name = format!("extra-{index}");
                    config.registrations.push(registration);
                }
            }
            Self::RegistrationProtocol => {
                config.transports.push(secure_transport()?);
                first(&mut config.registrations)?.transport = "secure".into();
            }
            Self::RegistrationContactUser => {
                first(&mut config.registrations)?.contact_user = "unsafe@host".into();
            }
            Self::WebSocketCredentialPair => {
                let credential = first(&mut config.users)?.password.clone();
                first(&mut config.websocket.clients)?.username = Some(credential);
            }
            Self::WebSocketBudget => {
                first(&mut config.websocket.clients)?.connection_timeout_ms = 4000;
            }
            Self::WebSocketPlaintextCa => {
                first(&mut config.websocket.clients)?.ca_list_file = Some("roots.pem".into());
            }
            Self::WebSocketUri => {
                let credential = first(&mut config.users)?.password.clone();
                first(&mut config.websocket.clients)?.uri = credential;
            }
            Self::WebSocketCapacity => {
                let prototype = first(&mut config.websocket.clients)?.clone();
                for index in config.websocket.clients.len()..=64 {
                    let mut client = prototype.clone();
                    client.name = format!("media-{index}");
                    config.websocket.clients.push(client);
                }
            }
        }
        Ok(())
    }

    const fn file(self) -> &'static str {
        match self {
            Self::RegistrationCapacity
            | Self::RegistrationProtocol
            | Self::RegistrationContactUser => "pjsip.conf",
            Self::WebSocketCredentialPair
            | Self::WebSocketBudget
            | Self::WebSocketPlaintextCa
            | Self::WebSocketUri
            | Self::WebSocketCapacity => "websocket_client.conf",
        }
    }
}

#[test]
fn runtime_validation_checks_registration_and_media_profiles_before_io() -> Result<(), ConfigError>
{
    for mutation in ProfileMutation::ALL {
        let mut candidate = configuration()?;
        mutation.apply(&mut candidate)?;
        refused(&candidate, mutation.file())
            .map_err(|error| missing(format!("{mutation:?}: {error}")))?;
    }
    Ok(())
}

#[test]
fn runtime_validation_checks_owned_application_values_without_text_reconstruction(
) -> Result<(), ConfigError> {
    for application in [
        Application::Dial {
            destinations: Vec::new(),
            timeout: None,
        },
        Application::Dial {
            destinations: ["PJSIP/phone".into()].into(),
            timeout: Some(0),
        },
        Application::Dial {
            destinations: ["SIP/phone".into()].into(),
            timeout: None,
        },
        Application::Playback("invalid name".into()),
        Application::Stasis {
            application: "invalid name".into(),
            arguments: Vec::new(),
        },
        Application::Set {
            variable: "INVALID()".into(),
            value: "value".into(),
        },
        Application::NoOp("${INVALID()}".into()),
        Application::Goto {
            context: Some("internal".into()),
            extension: None,
            priority: 1,
        },
    ] {
        let mut candidate = configuration()?;
        first(&mut candidate.dialplan.steps)?.application = application;
        refused(&candidate, "extensions.conf")?;
    }
    Ok(())
}

#[test]
fn runtime_validation_bounds_steps_and_refuses_duplicate_priority_keys() -> Result<(), ConfigError>
{
    let mut duplicate = configuration()?;
    let step = first(&mut duplicate.dialplan.steps)?.clone();
    duplicate.dialplan.steps.push(step.clone());
    refused(&duplicate, "extensions.conf")?;
    let mut excessive = configuration()?;
    for index in excessive.dialplan.steps.len()..=4096 {
        let mut additional = step.clone();
        additional.extension = format!("extension-{index}");
        excessive.dialplan.steps.push(additional);
    }
    refused(&excessive, "extensions.conf")?;
    Ok(())
}

#[test]
fn runtime_validation_preserves_existing_application_and_shared_object_namespaces(
) -> Result<(), ConfigError> {
    let mut candidate = configuration()?;
    first(&mut candidate.aors)?.max_contacts = 0;
    first(&mut candidate.endpoints)?.name = "credential".into();
    candidate.dialplan = Dialplan::parse(
        "[internal]\nexten => 100,1,NoOp(${CALLERID(num)})\nsame => n,Set(CALLERID(num)=value)\nsame => n,Dial(PJSIP/phone)\nsame => n,Goto(0)\n",
    )?;
    candidate.validate_runtime()
}

#[test]
fn runtime_validation_keeps_tls_file_io_outside_the_pure_value_check() -> Result<(), ConfigError> {
    Config::parse(
        "[general]\ntlsenable=yes\ntlsbindaddr=127.0.0.1:8089\ntlscertfile=runtime-validation-missing/http.pem\ntlsprivatekey=runtime-validation-missing/http-key.pem\n",
        ARI,
        "[secure]\ntype=transport\nprotocol=tls\nbind=127.0.0.1:5061\ncert_file=runtime-validation-missing/sip.pem\npriv_key_file=runtime-validation-missing/sip-key.pem\nca_list_file=runtime-validation-missing/sip-roots.pem\n[browser]\ntype=transport\nprotocol=wss\nbind=127.0.0.1\n",
        "",
        "",
    )?
    .with_websocket_configuration(
        "",
        "[media]\ntype=websocket_client\nconnection_type=per_call_config\nuri=WsS://media.example/stream\nprotocols=media\ntls_enabled=yes\nca_list_file=runtime-validation-missing/media-roots.pem\n",
    )?
    .validate_runtime()
}
