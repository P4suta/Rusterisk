use crate::{Method, Operation, OPERATIONS};
use Method::{Delete, Get, Post, Put};

const UNAVAILABLE: &[(Method, &str)] = &[
    (
        Get,
        "/asterisk/config/dynamic/{configClass}/{objectType}/{id}",
    ),
    (
        Put,
        "/asterisk/config/dynamic/{configClass}/{objectType}/{id}",
    ),
    (
        Delete,
        "/asterisk/config/dynamic/{configClass}/{objectType}/{id}",
    ),
    (Get, "/asterisk/modules"),
    (Get, "/asterisk/modules/{moduleName}"),
    (Post, "/asterisk/modules/{moduleName}"),
    (Delete, "/asterisk/modules/{moduleName}"),
    (Put, "/asterisk/modules/{moduleName}"),
    (Get, "/asterisk/logging"),
    (Post, "/asterisk/logging/{logChannelName}"),
    (Delete, "/asterisk/logging/{logChannelName}"),
    (Put, "/asterisk/logging/{logChannelName}/rotate"),
    (Get, "/asterisk/variable"),
    (Post, "/asterisk/variable"),
    (Post, "/bridges/{bridgeId}/videoSource/{channelId}"),
    (Delete, "/bridges/{bridgeId}/videoSource"),
    (Post, "/bridges/{bridgeId}/moh"),
    (Delete, "/bridges/{bridgeId}/moh"),
    (Post, "/channels/{channelId}/progress"),
    (Post, "/channels/{channelId}/silence"),
    (Delete, "/channels/{channelId}/silence"),
    (Post, "/channels/{channelId}/snoop"),
    (Post, "/channels/{channelId}/snoop/{snoopId}"),
    (Get, "/channels/{channelId}/rtp_statistics"),
    (Post, "/channels/{channelId}/transfer_progress"),
    (Get, "/deviceStates"),
    (Get, "/deviceStates/{deviceName}"),
    (Put, "/deviceStates/{deviceName}"),
    (Delete, "/deviceStates/{deviceName}"),
    (Put, "/endpoints/sendMessage"),
    (Post, "/endpoints/refer"),
    (Put, "/endpoints/{tech}/{resource}/sendMessage"),
    (Post, "/endpoints/{tech}/{resource}/refer"),
    (Post, "/events/user/{eventName}"),
    (Get, "/mailboxes"),
    (Get, "/mailboxes/{mailboxName}"),
    (Put, "/mailboxes/{mailboxName}"),
    (Delete, "/mailboxes/{mailboxName}"),
];

pub enum AriRoute {
    Declared(&'static Operation),
    Unsupported,
    OtherMethods(String),
    NotFound,
}

struct Candidate {
    segments: std::str::Split<'static, char>,
    method: Method,
    operation: Option<&'static Operation>,
}

impl Candidate {
    fn new(method: Method, path: &'static str, operation: Option<&'static Operation>) -> Self {
        Self {
            segments: path.trim_start_matches('/').split('/'),
            method,
            operation,
        }
    }
}

pub fn ari_route<'a>(method: &str, mut segments: impl Iterator<Item = &'a str>) -> AriRoute {
    let Some(resource) = segments.next() else {
        return AriRoute::NotFound;
    };
    let mut candidates: Vec<_> = OPERATIONS
        .iter()
        .map(|op| Candidate::new(op.method, op.path, Some(op)))
        .chain(
            UNAVAILABLE
                .iter()
                .map(|&(method, path)| Candidate::new(method, path, None)),
        )
        .filter_map(|mut candidate| {
            (candidate.segments.next() == Some(resource)).then_some(candidate)
        })
        .collect();
    if candidates.is_empty() {
        return AriRoute::NotFound;
    }
    for segment in segments {
        if segment.is_empty() {
            return AriRoute::NotFound;
        }
        let literal = candidates
            .iter()
            .any(|candidate| candidate.segments.clone().next() == Some(segment));
        candidates.retain_mut(|candidate| {
            candidate
                .segments
                .next()
                .is_some_and(|part| part == segment || (!literal && part.starts_with('{')))
        });
        if candidates.is_empty() {
            return AriRoute::NotFound;
        }
    }
    candidates.retain_mut(|candidate| candidate.segments.next().is_none());
    if let Some(candidate) = candidates
        .iter()
        .find(|candidate| candidate.method.upper() == method)
    {
        return candidate
            .operation
            .map_or(AriRoute::Unsupported, AriRoute::Declared);
    }
    let mut allow = String::from("OPTIONS");
    for method in [Get, Post, Put, Delete] {
        // Asterisk handles the event WebSocket upgrade outside its HTTP method callbacks.
        if candidates.iter().any(|candidate| {
            candidate.method == method
                && candidate
                    .operation
                    .is_none_or(|op| op.id != "events.eventWebsocket")
        }) {
            allow.push(',');
            allow.push_str(method.upper());
        }
    }
    AriRoute::OtherMethods(allow)
}

pub fn is_known_operation(method: &str, path: &str) -> bool {
    path.strip_prefix('/').is_some_and(|path| {
        matches!(
            ari_route(method, path.split('/')),
            AriRoute::Declared(_) | AriRoute::Unsupported
        )
    })
}
