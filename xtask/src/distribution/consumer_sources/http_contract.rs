use rusterisk_client::{
    api::{Bridge, ChannelId, InfoSection},
    Client, Error,
};
use std::time::Duration;

#[derive(Clone, Copy, Debug)]
enum Operation {
    Ping,
    Info,
    Preflight,
    Create,
    Inspect,
    List,
    Delete,
    Absent,
    FinalList,
    Workflow,
    Cleanup,
}

impl Operation {
    fn label(self) -> &'static str {
        match self {
            Self::Ping => "ping",
            Self::Info => "server information",
            Self::Preflight => "bridge preflight",
            Self::Create => "create bridge",
            Self::Inspect => "inspect bridge",
            Self::List => "list bridges",
            Self::Delete => "delete bridge",
            Self::Absent => "confirm bridge absence",
            Self::FinalList => "confirm list absence",
            Self::Workflow => "request workflow",
            Self::Cleanup => "cleanup workflow",
        }
    }
}

#[derive(Debug)]
enum FailureKind {
    Status(u16),
    Request,
    Response,
    Deadline,
}

#[derive(Debug)]
struct Failure {
    operation: Operation,
    kind: FailureKind,
}

impl Failure {
    fn request(operation: Operation, error: Error) -> Self {
        Self {
            operation,
            kind: match error {
                Error::Api { status, .. } => FailureKind::Status(status),
                Error::Invalid(_)
                | Error::Id(_)
                | Error::Io(_)
                | Error::Transport(_)
                | Error::Json(_)
                | Error::Capacity
                | Error::Closed
                | Error::Timeout
                | Error::WebSocket(_)
                | Error::EventLagged(_)
                | Error::CallEnded(_)
                | Error::StasisExited
                | Error::MediaProbe(_)
                | Error::CallFailed(_)
                | Error::Cleanup(_) => FailureKind::Request,
            },
        }
    }

    fn response(operation: Operation) -> Self {
        Self {
            operation,
            kind: FailureKind::Response,
        }
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: ", self.operation.label())?;
        match self.kind {
            FailureKind::Status(status) => write!(formatter, "HTTP {status}"),
            FailureKind::Request => formatter.write_str("request failed"),
            FailureKind::Response => formatter.write_str("response contract failed"),
            FailureKind::Deadline => formatter.write_str("deadline expired"),
        }
    }
}

#[derive(Debug)]
pub struct HttpContractError {
    operation: Option<Failure>,
    cleanup: Option<Failure>,
}

impl std::fmt::Display for HttpContractError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(operation) = &self.operation {
            write!(formatter, "SDK HTTP {operation}")?;
        }
        if let Some(cleanup) = &self.cleanup {
            if self.operation.is_some() {
                formatter.write_str("; ")?;
            }
            write!(formatter, "SDK HTTP cleanup {cleanup}")?;
        }
        Ok(())
    }
}
impl std::error::Error for HttpContractError {}

enum Reservation {
    Unclaimed,
    Attempted,
}

fn bridge_matches(bridge: &Bridge, identifier: &ChannelId) -> bool {
    bridge.id == *identifier
        && bridge.name == "SDK distribution consumer"
        && bridge.channels.is_empty()
}

async fn operation(
    client: &Client,
    version: &str,
    identifier: &ChannelId,
    reservation: &mut Reservation,
) -> Result<(), Failure> {
    let ping = client
        .ping()
        .await
        .map_err(|error| Failure::request(Operation::Ping, error))?;
    if ping.ping != "pong" || ping.timestamp.is_empty() || ping.asterisk_id.is_empty() {
        return Err(Failure::response(Operation::Ping));
    }
    let info = client
        .server_info(&[InfoSection::System, InfoSection::Status])
        .await
        .map_err(|error| Failure::request(Operation::Info, error))?;
    if info.build.is_some()
        || info.config.is_some()
        || !info
            .system
            .as_ref()
            .is_some_and(|system| system.version == version && !system.entity_id.is_empty())
        || !info.status.as_ref().is_some_and(|status| {
            !status.startup_time.is_empty() && !status.last_reload_time.is_empty()
        })
    {
        return Err(Failure::response(Operation::Info));
    }
    match client.bridge(identifier).await {
        Err(Error::Api { status: 404, .. }) => {}
        Err(error) => return Err(Failure::request(Operation::Preflight, error)),
        Ok(_) => return Err(Failure::response(Operation::Preflight)),
    }
    *reservation = Reservation::Attempted;
    let created = match client
        .create_bridge(identifier, "SDK distribution consumer")
        .await
    {
        Ok(bridge) => bridge,
        Err(error @ Error::Api { status: 409, .. }) => {
            *reservation = Reservation::Unclaimed;
            return Err(Failure::request(Operation::Create, error));
        }
        Err(error) => return Err(Failure::request(Operation::Create, error)),
    };
    if !bridge_matches(&created, identifier) {
        return Err(Failure::response(Operation::Create));
    }
    let inspected = client
        .bridge(identifier)
        .await
        .map_err(|error| Failure::request(Operation::Inspect, error))?;
    if !bridge_matches(&inspected, identifier) {
        return Err(Failure::response(Operation::Inspect));
    }
    let bridges = client
        .bridges()
        .await
        .map_err(|error| Failure::request(Operation::List, error))?;
    let mut matching = bridges.iter().filter(|bridge| bridge.id == *identifier);
    if !matching
        .next()
        .is_some_and(|bridge| bridge_matches(bridge, identifier))
        || matching.next().is_some()
    {
        return Err(Failure::response(Operation::List));
    }
    Ok(())
}

async fn cleanup(client: &Client, identifier: &ChannelId) -> Result<(), Failure> {
    match client.destroy_bridge(identifier).await {
        Ok(()) | Err(Error::Api { status: 404, .. }) => {}
        Err(error) => return Err(Failure::request(Operation::Delete, error)),
    }
    match client.bridge(identifier).await {
        Err(Error::Api { status: 404, .. }) => {}
        Err(error) => return Err(Failure::request(Operation::Absent, error)),
        Ok(_) => return Err(Failure::response(Operation::Absent)),
    }
    let bridges = client
        .bridges()
        .await
        .map_err(|error| Failure::request(Operation::FinalList, error))?;
    if bridges.iter().any(|bridge| bridge.id == *identifier) {
        return Err(Failure::response(Operation::FinalList));
    }
    Ok(())
}

pub async fn verify(
    client: &Client,
    version: &str,
    identifier: &ChannelId,
) -> Result<(), HttpContractError> {
    let mut reservation = Reservation::Unclaimed;
    let operation = match tokio::time::timeout(
        Duration::from_secs(30),
        operation(client, version, identifier, &mut reservation),
    )
    .await
    {
        Ok(result) => result.err(),
        Err(_) => Some(Failure {
            operation: Operation::Workflow,
            kind: FailureKind::Deadline,
        }),
    };
    let cleanup = match reservation {
        Reservation::Unclaimed => None,
        Reservation::Attempted => {
            match tokio::time::timeout(Duration::from_secs(20), cleanup(client, identifier)).await {
                Ok(result) => result.err(),
                Err(_) => Some(Failure {
                    operation: Operation::Cleanup,
                    kind: FailureKind::Deadline,
                }),
            }
        }
    };
    if operation.is_some() || cleanup.is_some() {
        Err(HttpContractError { operation, cleanup })
    } else {
        Ok(())
    }
}
