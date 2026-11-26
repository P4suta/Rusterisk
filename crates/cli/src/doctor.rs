use crate::{http::Remote, Error};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::time::Duration;

pub enum Doctor {
    Runtime,
    Extension { context: String, extension: String },
    Channel(String),
}

#[derive(Serialize, Deserialize)]
struct RuntimeActivity {
    control: Activity,
    signaling: Activity,
    media: Activity,
}

#[derive(Serialize, Deserialize)]
struct Activity {
    turns: u64,
    late_turns: u64,
    drops: u64,
}

#[derive(Deserialize)]
struct Channel {
    id: String,
    state: String,
    creationtime: String,
    dialplan: Option<Location>,
}
#[derive(Deserialize)]
struct Location {
    context: String,
    exten: String,
}
#[derive(Deserialize)]
struct Signaling {
    id: String,
    creationtime: String,
    generation: u64,
    held: bool,
    hold_source: Option<String>,
    endpoint: String,
}
#[derive(Deserialize)]
struct Sample {
    channel_id: String,
    creationtime: String,
    generation: u64,
    observed_at: String,
    local: Option<Counts>,
}
#[derive(Deserialize)]
struct Counts {
    source: String,
    rxcount: u64,
    txcount: u64,
}

#[derive(Serialize)]
struct Traffic {
    observation: &'static str,
    interval_ms: Option<u64>,
    start: Option<String>,
    end: Option<String>,
    received: Option<u64>,
    sent: Option<u64>,
}
impl Traffic {
    const fn unavailable(reason: &'static str) -> Self {
        Self {
            observation: reason,
            interval_ms: None,
            start: None,
            end: None,
            received: None,
            sent: None,
        }
    }
}
#[derive(Serialize)]
struct Report {
    id: String,
    state: String,
    creationtime: String,
    held: Option<bool>,
    hold_source: Option<String>,
    endpoint: Option<String>,
    traffic: Traffic,
    next_check: &'static str,
}

#[derive(Serialize, Deserialize)]
struct Step {
    context: String,
    pattern: String,
    priority: u16,
    operation: String,
}
#[derive(Serialize, Deserialize)]
struct Destination {
    endpoint: Option<String>,
    static_contact: Option<bool>,
    registration: String,
    available: Option<bool>,
}
#[derive(Serialize, Deserialize)]
struct Route {
    context: String,
    extension: String,
    steps: Vec<Step>,
    disposition: String,
    application: Option<String>,
    destinations: Vec<Destination>,
    channels: Vec<String>,
}
#[derive(Serialize)]
struct RouteReport {
    #[serde(flatten)]
    route: Route,
    next_check: &'static str,
}

async fn optional<T: DeserializeOwned>(
    remote: &Remote<'_>,
    path: &[&str],
) -> Result<Option<T>, Error> {
    match remote.get(path).await {
        Ok(value) => Ok(Some(value)),
        Err(Error::Remote(status, _)) if matches!(status.as_u16(), 404 | 501) => Ok(None),
        Err(error) => Err(error),
    }
}
fn encode(value: &impl Serialize) -> Result<String, Error> {
    serde_json::to_string(value).map_err(|_| Error::Response("Could not encode the diagnosis"))
}

async fn route(remote: &Remote<'_>, context: &str, extension: &str) -> Result<RouteReport, Error> {
    let resolved: Option<Route> = optional(
        remote,
        &["rusterisk", "diagnostics", "extensions", context, extension],
    )
    .await?;
    let route = match resolved {
        Some(route) if route.context == context && route.extension == extension => route,
        Some(_) => {
            return Err(Error::Response(
                "Dialplan location differs from the request",
            ))
        }
        None => {
            let channels: Vec<Channel> = remote.get(&["ari", "channels"]).await?;
            let channels = channels
                .into_iter()
                .filter(|leg| {
                    leg.dialplan.as_ref().is_some_and(|location| {
                        location.context == context && location.exten == extension
                    })
                })
                .map(|leg| leg.id)
                .collect();
            Route {
                context: context.into(),
                extension: extension.into(),
                steps: Vec::new(),
                disposition: "unavailable".into(),
                application: None,
                destinations: Vec::new(),
                channels,
            }
        }
    };
    let next_check = match route.disposition.as_str() {
        "stasis" => "Check the application's event connection and the destination it selected.",
        "missing" => "Check the extension or pattern at priority 1 in this context.",
        "dial"
            if route
                .destinations
                .iter()
                .any(|d| d.available == Some(false)) =>
        {
            "Check the unavailable endpoint's configured contact or current registration."
        }
        "dial" => "Check the destination's SIP response to the invitation.",
        "loop" | "limit" => {
            "Inspect the sequence of priorities and Goto targets in the reported route."
        }
        "unavailable" => "Inspect the server dialplan for this context and extension.",
        "dynamic" => "Inspect the call variables used to select the next destination.",
        _ => "Inspect the final application and following dialplan priority.",
    };
    Ok(RouteReport { route, next_check })
}

fn changes(first: Sample, last: Sample) -> Traffic {
    if first.channel_id != last.channel_id
        || first.creationtime != last.creationtime
        || first.generation != last.generation
    {
        return Traffic::unavailable("lifetime_changed");
    }
    let (Some(a), Some(b)) = (first.local, last.local) else {
        return Traffic::unavailable("unavailable");
    };
    if a.source != "server_rtp" || b.source != "server_rtp" {
        return Traffic::unavailable("unavailable");
    }
    let (Ok(start), Ok(end)) = (
        first.observed_at.parse::<jiff::Timestamp>(),
        last.observed_at.parse::<jiff::Timestamp>(),
    ) else {
        return Traffic::unavailable("invalid_interval");
    };
    let interval = end.duration_since(start);
    let Ok(interval_ms) = u64::try_from(interval.as_millis()) else {
        return Traffic::unavailable("invalid_interval");
    };
    if interval_ms == 0 {
        return Traffic::unavailable("invalid_interval");
    }
    let (Some(rx), Some(tx)) = (
        b.rxcount.checked_sub(a.rxcount),
        b.txcount.checked_sub(a.txcount),
    ) else {
        return Traffic::unavailable("counter_reset");
    };
    let observation = match (rx > 0, tx > 0) {
        (false, false) => "idle",
        (true, true) => "both_directions",
        (true, false) => "received_only",
        (false, true) => "sent_only",
    };
    Traffic {
        observation,
        interval_ms: Some(interval_ms),
        start: Some(first.observed_at),
        end: Some(last.observed_at),
        received: Some(rx),
        sent: Some(tx),
    }
}

async fn packet_sample(remote: &Remote<'_>, path: &[&str]) -> Result<Option<Sample>, Error> {
    match optional(remote, path).await {
        Err(Error::Remote(status, _)) if status.is_server_error() => Ok(None),
        Err(Error::Transport(_)) => Ok(None),
        result => result,
    }
}

async fn traffic(
    remote: &Remote<'_>,
    channel: &Channel,
    generation: Option<u64>,
) -> Result<Traffic, Error> {
    let path = ["rusterisk", "channels", &channel.id, "rtp-statistics"];
    let Some(first) = packet_sample(remote, &path).await? else {
        return Ok(Traffic::unavailable("unavailable"));
    };
    if first.channel_id != channel.id
        || first.creationtime != channel.creationtime
        || generation.is_some_and(|g| g != first.generation)
    {
        return Ok(Traffic::unavailable("lifetime_changed"));
    }
    if first.local.is_none() {
        return Ok(Traffic::unavailable("unavailable"));
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    let Some(last) = packet_sample(remote, &path).await? else {
        return Ok(Traffic::unavailable("unavailable"));
    };
    Ok(changes(first, last))
}

async fn channel(remote: &Remote<'_>, id: &str) -> Result<Report, Error> {
    let leg: Channel = remote.get(&["ari", "channels", id]).await?;
    if leg.id != id {
        return Err(Error::Response("Channel identity differs from the request"));
    }
    let signaling: Option<Signaling> =
        optional(remote, &["rusterisk", "diagnostics", "channels", id]).await?;
    if signaling
        .as_ref()
        .is_some_and(|s| s.id != id || s.creationtime != leg.creationtime)
    {
        return Err(Error::Response("Channel lifetime changed during diagnosis"));
    }
    let held = signaling.as_ref().map(|s| s.held);
    let hold_source = signaling.as_ref().and_then(|s| s.hold_source.clone());
    let traffic = traffic(remote, &leg, signaling.as_ref().map(|s| s.generation)).await?;
    let next_check = match (held, traffic.observation) {
        (Some(true), _) if hold_source.as_deref() == Some("peer") => {
            "Check the direction negotiated by the remote endpoint."
        }
        (Some(true), _) => "Check the application that placed the channel on hold.",
        (_, "received_only") => {
            "Inspect the server's outbound media route and the peer's receiver."
        }
        (_, "sent_only") => "Inspect the peer's transmitter and the server's inbound media route.",
        (_, "both_directions") => "Verify the received signal at the intended endpoint.",
        (_, "idle") => "Check the negotiated media address and direction at both ends.",
        (_, "lifetime_changed" | "counter_reset" | "invalid_interval") => {
            "Start another sample against the current channel generation."
        }
        _ => "Check whether the server and transport expose packet measurements for this channel.",
    };
    Ok(Report {
        id: leg.id,
        state: leg.state,
        creationtime: leg.creationtime,
        held,
        hold_source,
        endpoint: signaling.map(|s| s.endpoint),
        traffic,
        next_check,
    })
}

impl Doctor {
    pub(super) async fn read(&self, remote: &Remote<'_>, json: bool) -> Result<String, Error> {
        match self {
            Self::Runtime => {
                let runtime: RuntimeActivity =
                    remote.get(&["rusterisk", "diagnostics", "runtime"]).await?;
                if json {
                    return encode(&runtime);
                }
                let rows = [
                    ("control", runtime.control),
                    ("signaling", runtime.signaling),
                    ("media", runtime.media),
                ]
                .map(|(name, activity)| {
                    format!(
                        "{name}  {}  {}  {}",
                        activity.turns, activity.late_turns, activity.drops
                    )
                });
                Ok(format!(
                    "component  turns  late turns  drops\n{}",
                    rows.join("\n")
                ))
            }
            Self::Channel(id) => {
                let report = channel(remote, id).await?;
                if json {
                    return encode(&report);
                }
                let t = &report.traffic;
                let mut text = format!(
                    "Call leg {}  state {}  hold {}\nServer packet observation  {}",
                    report.id.escape_debug(),
                    report.state.escape_debug(),
                    match report.held {
                        Some(true) => "held",
                        Some(false) => "released",
                        None => "unknown",
                    },
                    t.observation
                );
                if let (Some(ms), Some(rx), Some(tx)) = (t.interval_ms, t.received, t.sent) {
                    text.push_str(&format!("  {ms} ms  received {rx}  sent {tx}"));
                }
                if let Some(source) = &report.hold_source {
                    text.push_str(&format!("\nHold source  {}", source.escape_debug()));
                }
                text.push_str(&format!("\nNext check  {}", report.next_check));
                Ok(text)
            }
            Self::Extension { context, extension } => {
                let report = route(remote, context, extension).await?;
                if json {
                    return encode(&report);
                }
                let r = &report.route;
                let mut lines = Vec::new();
                lines.push(format!(
                    "Dialplan {}/{}  {}",
                    r.context.escape_debug(),
                    r.extension.escape_debug(),
                    r.disposition.escape_debug()
                ));
                for step in &r.steps {
                    lines.push(format!(
                        "  {}/{},{}  {}",
                        step.context.escape_debug(),
                        step.pattern.escape_debug(),
                        step.priority,
                        step.operation.escape_debug()
                    ));
                }
                if let Some(app) = &r.application {
                    lines.push(format!("Application  {}", app.escape_debug()));
                }
                for d in &r.destinations {
                    lines.push(format!(
                        "Endpoint {}  route {}  static contact {}  registration {}",
                        d.endpoint.as_deref().unwrap_or("unknown").escape_debug(),
                        match d.available {
                            Some(true) => "available",
                            Some(false) => "unavailable",
                            None => "unknown",
                        },
                        match d.static_contact {
                            Some(true) => "yes",
                            Some(false) => "no",
                            None => "unknown",
                        },
                        d.registration.escape_debug()
                    ));
                }
                lines.push(format!(
                    "Call legs  [{}]",
                    r.channels.join(", ").escape_debug()
                ));
                lines.push(format!("Next check  {}", report.next_check));
                Ok(lines.join("\n"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(rx: u64, tx: u64, generation: u64) -> Sample {
        Sample {
            channel_id: "leg".into(),
            creationtime: "start".into(),
            generation,
            observed_at: "2026-11-21T10:00:00.000Z".into(),
            local: Some(Counts {
                source: "server_rtp".into(),
                rxcount: rx,
                txcount: tx,
            }),
        }
    }

    #[test]
    fn packet_interval_uses_the_two_capture_times() {
        let mut first = sample(4, 7, 1);
        first.observed_at = "2026-11-21T10:00:00.000Z".into();
        let mut last = sample(5, 9, 1);
        last.observed_at = "2026-11-21T11:00:02.250+01:00".into();
        let span = changes(first, last);
        assert_eq!(span.interval_ms, Some(2250));
    }

    #[test]
    fn invalid_or_non_increasing_capture_times_leave_the_interval_unavailable() {
        for end in ["invalid", "2026-11-21T09:59:59Z", "2026-11-21T10:00:00Z"] {
            let mut last = sample(5, 9, 1);
            last.observed_at = end.into();
            let traffic = changes(sample(4, 7, 1), last);
            assert_eq!(traffic.observation, "invalid_interval");
            assert!(traffic.interval_ms.is_none());
            assert!(traffic.received.is_none());
        }
    }

    #[test]
    fn packet_changes_require_one_lifetime_and_monotonic_counters() {
        for (rx, tx, generation, expected) in [
            (4, 7, 1, "idle"),
            (5, 7, 1, "received_only"),
            (4, 9, 1, "sent_only"),
            (9, 11, 1, "both_directions"),
            (4, 10, 2, "lifetime_changed"),
            (3, 10, 1, "counter_reset"),
        ] {
            let mut last = sample(rx, tx, generation);
            last.observed_at = "2026-11-21T10:00:00.505Z".into();
            let result = changes(sample(4, 7, 1), last);
            assert_eq!(result.observation, expected);
            if let Some(interval) = result.interval_ms {
                assert_eq!(interval, 505);
                assert_eq!(result.received, Some(rx - 4));
                assert_eq!(result.sent, Some(tx - 7));
            }
        }
    }
}
