use super::Scope;
use std::{collections::BTreeSet, io};

fn invalid() -> io::Error {
    io::Error::other("incomplete native sanitizer execution")
}

fn text(bytes: &[u8]) -> io::Result<&str> {
    if bytes.len() > 4 * 1024 * 1024 {
        return Err(invalid());
    }
    std::str::from_utf8(bytes).map_err(|_| invalid())
}

fn required(scope: Scope) -> &'static [&'static str] {
    match scope {
        Scope::Media => &[
            "opus_transmits_a_non_silent_known_signal",
            "opus_packets_cover_each_frame_duration_and_the_full_120_millisecond_packet",
            "opus_packet_durations_preserve_decoded_samples_in_complete_mixer_frames",
        ],
        Scope::Storage => &[
            "history_preserves_channel_identity_across_sessions_and_commits_each_batch_atomically",
            "restart_interrupts_active_calls_and_repairs_recording_headers",
            "failed_replacement_publication_keeps_recoverable_audio_and_other_policies_cannot_replace_it",
        ],
        Scope::Tls => &[
            "signaling::stream_tests::native_tcp_and_tls_reuse_registered_flows_for_calls_and_release_disconnected_calls",
            "signaling::stream_tests::initial_outbound_connections_use_tcp_or_verified_tls_and_fail_closed",
        ],
    }
}

fn harnesses(scope: Scope) -> usize {
    match scope {
        Scope::Media | Scope::Storage => 3,
        Scope::Tls => 1,
    }
}

fn count(value: &str, suffix: &str) -> io::Result<usize> {
    value
        .strip_suffix(suffix)
        .ok_or_else(invalid)?
        .parse()
        .map_err(|_| invalid())
}

pub struct NativeInventory {
    scope: Scope,
    names: BTreeSet<String>,
}

impl NativeInventory {
    pub fn parse(scope: Scope, bytes: &[u8]) -> io::Result<Self> {
        let mut names = BTreeSet::new();
        let mut summaries = 0usize;
        let mut total = 0usize;
        for line in text(bytes)?.lines() {
            if let Some(name) = line.strip_suffix(": test") {
                if name.is_empty()
                    || name.len() > 256
                    || !name
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b':'))
                    || names.len() >= 1024
                    || !names.insert(name.to_owned())
                {
                    return Err(invalid());
                }
            } else if let Some(tests) = line.strip_suffix(", 0 benchmarks") {
                let tests = tests
                    .strip_suffix(" tests")
                    .or_else(|| tests.strip_suffix(" test"))
                    .ok_or_else(invalid)?;
                let tests = tests.parse::<usize>().map_err(|_| invalid())?;
                if tests == 0 {
                    return Err(invalid());
                }
                total = total.checked_add(tests).ok_or_else(invalid)?;
                summaries += 1;
            }
        }
        if summaries != harnesses(scope)
            || names.is_empty()
            || names.len() != total
            || required(scope).iter().any(|name| !names.contains(*name))
        {
            return Err(invalid());
        }
        Ok(Self { scope, names })
    }

    pub fn validate(&self, bytes: &[u8]) -> io::Result<usize> {
        let text = text(bytes)?;
        if text.contains("ERROR: AddressSanitizer")
            || text.contains("LeakSanitizer:")
            || text.contains("runtime error:")
            || text.contains("UndefinedBehaviorSanitizer:")
        {
            return Err(invalid());
        }
        let mut observed = BTreeSet::new();
        let mut summaries = 0usize;
        let mut passed = 0usize;
        for line in text.lines() {
            if let Some(summary) = line.strip_prefix("test result: ok. ") {
                let (summary, _) = summary.split_once("; finished in ").ok_or_else(invalid)?;
                let fields: Vec<_> = summary.split("; ").collect();
                if fields.len() != 5
                    || count(fields[1], " failed")? != 0
                    || count(fields[2], " ignored")? != 0
                    || count(fields[3], " measured")? != 0
                    || (count(fields[4], " filtered out")? != 0 && self.scope != Scope::Tls)
                {
                    return Err(invalid());
                }
                let tests = count(fields[0], " passed")?;
                if tests == 0 {
                    return Err(invalid());
                }
                passed = passed.checked_add(tests).ok_or_else(invalid)?;
                summaries += 1;
            } else if let Some(name) = line.strip_prefix("test ") {
                let name = name.strip_suffix(" ... ok").ok_or_else(invalid)?;
                if !self.names.contains(name) || !observed.insert(name.to_owned()) {
                    return Err(invalid());
                }
            }
        }
        if summaries != harnesses(self.scope)
            || passed != self.names.len()
            || observed != self.names
        {
            return Err(invalid());
        }
        Ok(passed)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Defect {
    Address,
    Undefined,
}

pub fn validate_rejection(defect: Defect, code: Option<i32>, bytes: &[u8]) -> io::Result<()> {
    let (expected, diagnostic) = match defect {
        Defect::Address => (86, "ERROR: AddressSanitizer: heap-use-after-free"),
        Defect::Undefined => (87, "runtime error: signed integer overflow"),
    };
    if code == Some(expected) && text(bytes)?.contains(diagnostic) {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "native {} probe requires exit {expected} and {diagnostic}; observed exit {code:?}",
            defect.label()
        )))
    }
}
