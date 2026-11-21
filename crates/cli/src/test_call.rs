use crate::{session, Error, RemoteOptions};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Outcome {
    Verified,
    Busy,
    NoAnswer,
    Silent,
    SignalMismatch,
    CallEnded,
    MediaUnavailable,
    NotAccepted,
}
impl Outcome {
    fn label(self) -> &'static str {
        match self {
            Self::Verified => "verified",
            Self::Busy => "busy",
            Self::NoAnswer => "no answer",
            Self::Silent => "silent",
            Self::SignalMismatch => "signal mismatch",
            Self::CallEnded => "call ended",
            Self::MediaUnavailable => "media unavailable",
            Self::NotAccepted => "not accepted",
        }
    }
}
#[derive(Serialize, Deserialize)]
struct ResultMessage {
    channel_id: String,
    accepted: bool,
    answered: bool,
    signal_verified: bool,
    outcome: Outcome,
}

pub fn test_call(
    options: &RemoteOptions,
    endpoint: &str,
    deadline: u16,
    json: bool,
) -> Result<(String, bool), Error> {
    if endpoint.is_empty()
        || endpoint.len() > 128
        || !endpoint
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
        || matches!(endpoint, "." | "..")
        || !(1..=120).contains(&deadline)
    {
        return Err(Error::Input("Invalid test endpoint or deadline"));
    }
    let (runtime, remote) = session(options)?;
    let report: ResultMessage = runtime.block_on(remote.post_test_call(endpoint, deadline))?;
    let verified = matches!(report.outcome, Outcome::Verified);
    if report.channel_id.is_empty()
        || report.signal_verified != verified
        || report.answered && !report.accepted
        || verified && !report.answered
    {
        return Err(Error::Response("Inconsistent test call result"));
    }
    let message = if json {
        serde_json::to_string(&report)
            .map_err(|_| Error::Response("Could not encode the test call result"))?
    } else {
        format!(
            "Test call {}\nAccepted  {}\nAnswered  {}\nReturned signal verified  {}\nOutcome  {}",
            report.channel_id.escape_debug(),
            report.accepted,
            report.answered,
            report.signal_verified,
            report.outcome.label()
        )
    };
    Ok((message, verified))
}
