use crate::{http::Remote, Error};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use std::io::Write;

pub enum Watch {
    Channel(String),
    Endpoint {
        technology: String,
        resource: String,
    },
}

#[derive(Serialize, Deserialize)]
struct Leg {
    id: String,
    generation: u64,
    state: String,
    endpoint: String,
    #[serde(default)]
    bridge_ids: Vec<String>,
}

#[derive(Serialize, Deserialize)]
struct Endpoint {
    technology: String,
    resource: String,
    state: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct Record {
    timestamp: String,
    #[serde(flatten)]
    entry: Entry,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Entry {
    Snapshot {
        channels: Vec<Leg>,
        endpoint: Option<Endpoint>,
    },
    Event {
        event: String,
        channel: Option<Leg>,
        endpoint: Option<Endpoint>,
        bridge_id: Option<String>,
    },
    Gap {
        reason: String,
        skipped: Option<u64>,
    },
}

impl Record {
    fn write(&self, json: bool, out: &mut impl Write) -> Result<(), Error> {
        if json {
            serde_json::to_writer(&mut *out, self)
                .map_err(|_| Error::Response("Could not write the observation"))?;
        } else {
            write!(out, "{}  ", self.timestamp.escape_debug()).map_err(Error::Runtime)?;
            match &self.entry {
                Entry::Snapshot { channels, endpoint } => {
                    write!(out, "Snapshot").map_err(Error::Runtime)?;
                    if let Some(endpoint) = endpoint {
                        write!(
                            out,
                            "  {}/{} {}",
                            endpoint.technology.escape_debug(),
                            endpoint.resource.escape_debug(),
                            endpoint
                                .state
                                .as_deref()
                                .unwrap_or("unknown")
                                .escape_debug()
                        )
                        .map_err(Error::Runtime)?;
                    }
                    for leg in channels {
                        write_leg(out, leg)?;
                    }
                }
                Entry::Event {
                    event,
                    channel,
                    endpoint,
                    bridge_id,
                } => {
                    write!(out, "{}", event.escape_debug()).map_err(Error::Runtime)?;
                    if let Some(leg) = channel {
                        write_leg(out, leg)?;
                    }
                    if let Some(endpoint) = endpoint {
                        write!(
                            out,
                            "  {}/{} {}",
                            endpoint.technology.escape_debug(),
                            endpoint.resource.escape_debug(),
                            endpoint
                                .state
                                .as_deref()
                                .unwrap_or("unknown")
                                .escape_debug()
                        )
                        .map_err(Error::Runtime)?;
                    }
                    if let Some(bridge) = bridge_id {
                        write!(out, "  bridge {}", bridge.escape_debug())
                            .map_err(Error::Runtime)?;
                    }
                }
                Entry::Gap { reason, skipped } => {
                    write!(out, "Gap  {}", reason.escape_debug()).map_err(Error::Runtime)?;
                    if let Some(count) = skipped {
                        write!(out, "  skipped {count}").map_err(Error::Runtime)?;
                    }
                }
            }
        }
        writeln!(out)
            .and_then(|()| out.flush())
            .map_err(Error::Runtime)
    }
}

fn write_leg(out: &mut impl Write, leg: &Leg) -> Result<(), Error> {
    write!(
        out,
        "  call leg {} generation {} {} endpoint {} bridges [{}]",
        leg.id.escape_debug(),
        leg.generation,
        leg.state.escape_debug(),
        leg.endpoint.escape_debug(),
        leg.bridge_ids.join(", ").escape_debug()
    )
    .map_err(Error::Runtime)
}

fn gap(reason: &str, json: bool, out: &mut impl Write) -> Result<(), Error> {
    Record {
        timestamp: Timestamp::now().to_string(),
        entry: Entry::Gap {
            reason: reason.to_owned(),
            skipped: None,
        },
    }
    .write(json, out)?;
    Err(Error::Response("Call observation ended with a gap"))
}

enum Phase {
    Opening,
    Channel(u64),
    Endpoint,
}
enum Progress {
    Continue,
    Terminal,
    Gap,
}

impl Watch {
    fn accept(&self, phase: &mut Phase, entry: &Entry) -> Option<Progress> {
        match (&*phase, self, entry) {
            (_, _, Entry::Gap { .. }) => Some(Progress::Gap),
            (
                Phase::Opening,
                Self::Channel(id),
                Entry::Snapshot {
                    channels,
                    endpoint: None,
                },
            ) => {
                let [leg] = channels.as_slice() else {
                    return None;
                };
                if &leg.id != id {
                    return None;
                }
                *phase = Phase::Channel(leg.generation);
                Some(Progress::Continue)
            }
            (
                Phase::Opening,
                Self::Endpoint {
                    technology,
                    resource,
                },
                Entry::Snapshot {
                    channels,
                    endpoint: Some(endpoint),
                },
            ) if endpoint.technology.eq_ignore_ascii_case(technology)
                && &endpoint.resource == resource
                && channels.iter().all(|leg| &leg.endpoint == resource) =>
            {
                *phase = Phase::Endpoint;
                Some(Progress::Continue)
            }
            (
                Phase::Channel(generation),
                Self::Channel(id),
                Entry::Event {
                    event,
                    channel: Some(leg),
                    endpoint: None,
                    ..
                },
            ) if &leg.id == id && &leg.generation == generation => {
                Some(if event == "ChannelDestroyed" {
                    Progress::Terminal
                } else {
                    Progress::Continue
                })
            }
            (
                Phase::Endpoint,
                Self::Endpoint {
                    technology,
                    resource,
                },
                Entry::Event {
                    channel, endpoint, ..
                },
            ) if (channel.is_some() || endpoint.is_some())
                && channel.as_ref().is_none_or(|leg| &leg.endpoint == resource)
                && endpoint.as_ref().is_none_or(|e| {
                    e.technology.eq_ignore_ascii_case(technology) && &e.resource == resource
                }) =>
            {
                Some(Progress::Continue)
            }
            _ => None,
        }
    }

    pub(super) async fn follow(
        &self,
        remote: &Remote<'_>,
        json: bool,
        out: &mut impl Write,
    ) -> Result<(), Error> {
        let mut response = match self {
            Self::Channel(id) => {
                remote
                    .stream(&["rusterisk", "observe", "channels", id])
                    .await?
            }
            Self::Endpoint {
                technology,
                resource,
            } => {
                remote
                    .stream(&["rusterisk", "observe", "endpoints", technology, resource])
                    .await?
            }
        };
        if !response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.split(';').next())
            .is_some_and(|s| s.trim().eq_ignore_ascii_case("application/x-ndjson"))
        {
            return Err(Error::Response("Expected an NDJSON observation stream"));
        }
        let mut phase = Phase::Opening;
        let mut line = Vec::new();
        while let Ok(Some(chunk)) = response.chunk().await {
            for piece in chunk.split_inclusive(|&byte| byte == b'\n') {
                if piece.len() > 1024 * 1024 - line.len() {
                    return gap("record_too_large", json, out);
                }
                line.extend_from_slice(piece);
                if line.last() != Some(&b'\n') {
                    continue;
                }
                let Ok(record) = serde_json::from_slice::<Record>(&line) else {
                    return gap("invalid_record", json, out);
                };
                if record.timestamp.parse::<Timestamp>().is_err() {
                    return gap("invalid_timestamp", json, out);
                }
                let Some(progress) = self.accept(&mut phase, &record.entry) else {
                    return gap("unexpected_observation", json, out);
                };
                record.write(json, out)?;
                match progress {
                    Progress::Terminal => return Ok(()),
                    Progress::Gap => {
                        return Err(Error::Response("Call observation ended with a gap"))
                    }
                    Progress::Continue => line.clear(),
                }
            }
        }
        gap("connection_lost", json, out)
    }
}
