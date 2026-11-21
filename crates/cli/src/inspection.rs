use crate::{http::Remote, Error};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub enum Inspection {
    Status,
    Endpoints,
    Endpoint {
        technology: String,
        resource: String,
    },
    Channels,
    Channel(String),
}

#[derive(Deserialize)]
struct Info {
    system: System,
    status: Status,
}

#[derive(Deserialize)]
struct System {
    version: String,
}

#[derive(Deserialize)]
struct Status {
    startup_time: String,
}

#[derive(Serialize)]
struct Server {
    version: String,
    startup_time: String,
}

#[derive(Default, Deserialize, Serialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
enum RoutingState {
    Online,
    Offline,
    #[default]
    Unknown,
}

impl RoutingState {
    const fn label(self) -> &'static str {
        match self {
            Self::Online => "online",
            Self::Offline => "offline",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Deserialize)]
struct Endpoint {
    technology: String,
    resource: String,
    state: Option<RoutingState>,
    channel_ids: Vec<String>,
}

#[derive(Serialize)]
struct EndpointView {
    technology: String,
    resource: String,
    routing_state: RoutingState,
    registration: &'static str,
    channel_ids: Vec<String>,
}

impl From<Endpoint> for EndpointView {
    fn from(endpoint: Endpoint) -> Self {
        Self {
            technology: endpoint.technology,
            resource: endpoint.resource,
            routing_state: endpoint.state.unwrap_or_default(),
            registration: "unknown",
            channel_ids: endpoint.channel_ids,
        }
    }
}

#[derive(Deserialize)]
struct Channel {
    id: String,
    name: String,
    state: String,
    creationtime: String,
}

#[derive(Deserialize)]
struct Bridge {
    id: String,
    channels: Vec<String>,
}

#[derive(Serialize)]
struct Leg {
    kind: &'static str,
    id: String,
    name: String,
    state: String,
    creationtime: String,
    bridge_ids: Vec<String>,
}

fn encode(value: &impl Serialize) -> Result<String, Error> {
    serde_json::to_string(value)
        .map_err(|_| Error::Response("Could not encode the inspection result"))
}

fn endpoint_text(endpoint: &EndpointView) -> String {
    format!(
        "{}/{}  routing {}  registration {}  channels [{}]",
        endpoint.technology.escape_debug(),
        endpoint.resource.escape_debug(),
        endpoint.routing_state.label(),
        endpoint.registration,
        endpoint.channel_ids.join(", ").escape_debug()
    )
}

fn leg_text(leg: &Leg) -> String {
    format!(
        "Call leg {}  {}  {}  created {}  bridges [{}]",
        leg.id.escape_debug(),
        leg.name.escape_debug(),
        leg.state.escape_debug(),
        leg.creationtime.escape_debug(),
        leg.bridge_ids.join(", ").escape_debug()
    )
}

fn list<T: Serialize>(
    items: &[T],
    json: bool,
    empty: &str,
    row: fn(&T) -> String,
) -> Result<String, Error> {
    if json {
        return encode(&items);
    }
    if items.is_empty() {
        return Ok(empty.to_owned());
    }
    Ok(items.iter().map(row).collect::<Vec<_>>().join("\n"))
}

impl Inspection {
    pub(super) async fn read(&self, remote: &Remote<'_>, json: bool) -> Result<String, Error> {
        match self {
            Self::Status => {
                let info: Info = remote.get(&["ari", "asterisk", "info"]).await?;
                let server = Server {
                    version: info.system.version,
                    startup_time: info.status.startup_time,
                };
                if json {
                    return encode(&server);
                }
                Ok(format!(
                    "Server version {}\nStarted {}",
                    server.version.escape_debug(),
                    server.startup_time.escape_debug()
                ))
            }
            Self::Endpoints => {
                let endpoints: Vec<Endpoint> = remote.get(&["ari", "endpoints"]).await?;
                let rows: Vec<_> = endpoints.into_iter().map(EndpointView::from).collect();
                list(&rows, json, "No endpoints.", endpoint_text)
            }
            Self::Endpoint {
                technology,
                resource,
            } => {
                let endpoint: Endpoint = remote
                    .get(&["ari", "endpoints", technology, resource])
                    .await?;
                if !endpoint.technology.eq_ignore_ascii_case(technology)
                    || endpoint.resource != *resource
                {
                    return Err(Error::Response(
                        "Endpoint identity differs from the request",
                    ));
                }
                let row = EndpointView::from(endpoint);
                if json {
                    encode(&row)
                } else {
                    Ok(endpoint_text(&row))
                }
            }
            Self::Channels => {
                let channels = remote.get(&["ari", "channels"]).await?;
                let legs = legs(remote, channels).await?;
                list(&legs, json, "No active call legs.", leg_text)
            }
            Self::Channel(id) => {
                let channel: Channel = remote.get(&["ari", "channels", id]).await?;
                if channel.id != *id {
                    return Err(Error::Response("Channel identity differs from the request"));
                }
                let row = leg(channel, &mut memberships(remote).await?);
                if json {
                    encode(&row)
                } else {
                    Ok(leg_text(&row))
                }
            }
        }
    }
}

async fn memberships(remote: &Remote<'_>) -> Result<BTreeMap<String, Vec<String>>, Error> {
    let bridges: Vec<Bridge> = remote.get(&["ari", "bridges"]).await?;
    let mut by_channel: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for bridge in bridges {
        for id in bridge.channels {
            by_channel.entry(id).or_default().push(bridge.id.clone());
        }
    }
    Ok(by_channel)
}

fn leg(channel: Channel, memberships: &mut BTreeMap<String, Vec<String>>) -> Leg {
    let mut bridge_ids = memberships.remove(&channel.id).unwrap_or_default();
    bridge_ids.sort();
    bridge_ids.dedup();
    Leg {
        kind: "call_leg",
        id: channel.id,
        name: channel.name,
        state: channel.state,
        creationtime: channel.creationtime,
        bridge_ids,
    }
}

async fn legs(remote: &Remote<'_>, channels: Vec<Channel>) -> Result<Vec<Leg>, Error> {
    if channels.is_empty() {
        return Ok(Vec::new());
    }
    let mut memberships = memberships(remote).await?;
    Ok(channels
        .into_iter()
        .map(|channel| leg(channel, &mut memberships))
        .collect())
}
