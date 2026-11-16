use super::{
    api, CallMilestone, CallSnapshot, Calls, Cleanup, Client, Entry, Error, OwnedCallProgress,
    Probe, Work,
};
use std::collections::BTreeMap;
use tokio::sync::{oneshot, watch};

#[tokio::test]
async fn early_events_preserve_their_state_and_merge_missing_response_variables(
) -> Result<(), Error> {
    for variables in [
        None,
        Some(BTreeMap::from([("shared".to_owned(), "event".to_owned())])),
    ] {
        let client = Client::new("http://127.0.0.1:9", "app", "password")?;
        let (_commands, mut calls) =
            Calls::new(client, api::ChannelId::try_from("app".to_owned())?);
        let observed: api::Channel = serde_json::from_value(serde_json::json!({
            "id":"external", "name":"UnicastRTP/peer", "state":"Up", "accountcode":"",
            "caller":{"name":"", "number":""}, "connected":{"name":"", "number":""},
            "dialplan":{"context":"default", "exten":"s", "priority":1},
            "creationtime":"now", "language":"en", "channelvars":variables
        }))?;
        let mut accepted = observed.clone();
        accepted.state = api::ChannelState::Down;
        accepted.channelvars = Some(BTreeMap::from([
            ("UNICASTRTP_LOCAL_PORT".into(), "12000".into()),
            ("shared".into(), "response".into()),
        ]));
        let (progress, received) = watch::channel(CallSnapshot {
            progress: OwnedCallProgress::new()
                .observe(CallMilestone::Stasis)
                .observe(CallMilestone::Answered),
            channel: Some(observed.clone()),
            failure: None,
            cleanup: Cleanup::Pending,
            media_probe_failure: None,
        });
        let (_lifetime, lifetime) = oneshot::channel();
        calls.entries.insert(
            observed.id.clone(),
            Entry {
                progress,
                lifetime,
                work: Work::Originating,
                probe: Probe::Finished,
            },
        );
        calls.originated(observed.id.clone(), Ok(accepted));
        let snapshot = received.borrow();
        assert!(snapshot.progress.has_reached(CallMilestone::Accepted));
        assert!(snapshot.progress.has_reached(CallMilestone::Answered));
        let channel = snapshot.channel.as_ref().ok_or(Error::Closed)?;
        assert_eq!(channel.state, api::ChannelState::Up);
        let actual = channel
            .channelvars
            .as_ref()
            .ok_or(Error::Invalid("missing response variables"))?;
        assert_eq!(
            actual.get("UNICASTRTP_LOCAL_PORT").map(String::as_str),
            Some("12000")
        );
        assert_eq!(
            actual.get("shared").map(String::as_str),
            Some(if variables.is_some() {
                "event"
            } else {
                "response"
            })
        );
        assert!(calls.originates.is_empty());
        assert!(calls.probes.is_empty());
        assert!(calls.cleanups.is_empty());
        drop(snapshot);
        let mut latest = observed;
        latest.channelvars = Some(BTreeMap::from([("shared".into(), "later-event".into())]));
        calls.event(&api::Event {
            application: "app".into(),
            timestamp: "later".into(),
            kind: api::EventKind::ChannelStateChange {
                channel: latest.clone(),
            },
        })?;
        let mut metadata = latest;
        metadata.channelvars = None;
        calls.event(&api::Event {
            application: "app".into(),
            timestamp: "last".into(),
            kind: api::EventKind::ChannelCallerId {
                channel: metadata,
                caller_presentation: 0,
                caller_presentation_txt: "allowed".into(),
            },
        })?;
        let final_snapshot = received.borrow();
        let final_variables = final_snapshot
            .channel
            .as_ref()
            .and_then(|channel| channel.channelvars.as_ref())
            .ok_or(Error::Invalid("later event removed response variables"))?;
        assert_eq!(
            final_variables
                .get("UNICASTRTP_LOCAL_PORT")
                .map(String::as_str),
            Some("12000")
        );
        assert_eq!(
            final_variables.get("shared").map(String::as_str),
            Some("later-event")
        );
    }
    Ok(())
}

#[test]
fn channel_variables_remain_bounded_across_repeated_partial_events() -> Result<(), Error> {
    let mut channel: api::Channel = serde_json::from_value(serde_json::json!({
        "id":"external", "name":"UnicastRTP/peer", "state":"Up", "accountcode":"",
        "caller":{"name":"", "number":""}, "connected":{"name":"", "number":""},
        "dialplan":{"context":"default", "exten":"s", "priority":1},
        "creationtime":"now", "language":"en"
    }))?;
    let mut snapshot = CallSnapshot {
        progress: OwnedCallProgress::new(),
        channel: None,
        failure: None,
        cleanup: Cleanup::Pending,
        media_probe_failure: None,
    };
    for index in 0..129 {
        channel.channelvars = Some(BTreeMap::from([(
            format!("variable-{index}"),
            "value".into(),
        )]));
        let observation = snapshot.observe_channel(&channel);
        if index == 128 {
            assert!(matches!(observation, Err(Error::Capacity)));
        } else {
            observation?;
        }
        let variables = snapshot
            .channel
            .as_ref()
            .and_then(|channel| channel.channelvars.as_ref())
            .ok_or(Error::Closed)?;
        assert!(variables.len() <= 128);
    }
    let before = snapshot.channel.clone();
    channel.channelvars = Some(BTreeMap::from([(
        "oversize".into(),
        "x".repeat(crate::MAX_RESPONSE),
    )]));
    assert!(matches!(
        snapshot.observe_channel(&channel),
        Err(Error::Capacity)
    ));
    assert_eq!(snapshot.channel, before);
    let mut initial = CallSnapshot {
        progress: OwnedCallProgress::new(),
        channel: None,
        failure: None,
        cleanup: Cleanup::Pending,
        media_probe_failure: None,
    };
    channel.channelvars = Some(
        (0..129)
            .map(|index| (format!("key-{index}"), String::new()))
            .collect(),
    );
    assert!(matches!(
        initial.observe_channel(&channel),
        Err(Error::Capacity)
    ));
    assert!(initial.channel.is_none());
    Ok(())
}

#[tokio::test]
async fn metadata_overflow_keeps_cleanup_and_stasis_exit_responsibilities_separate(
) -> Result<(), Error> {
    use crate::CallCompletion;
    use std::future::IntoFuture;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let (deleted, mut received_deletions) = tokio::sync::mpsc::channel(2);
    let app = axum::Router::new().route(
        "/ari/channels/external",
        axum::routing::delete(move || {
            let deleted = deleted.clone();
            async move {
                deleted
                    .try_send(())
                    .map_err(|_| axum::http::StatusCode::SERVICE_UNAVAILABLE)?;
                Ok::<_, axum::http::StatusCode>(axum::http::StatusCode::NO_CONTENT)
            }
        }),
    );
    let server = tokio::spawn(axum::serve(listener, app).into_future());
    let result = async {
        for terminal in [false, true] {
            let client = Client::new(&format!("http://{address}"), "app", "password")?;
            let (_commands, mut calls) =
                Calls::new(client, api::ChannelId::try_from("app".to_owned())?);
            let channel: api::Channel = serde_json::from_value(serde_json::json!({
                "id":"external", "name":"UnicastRTP/peer", "state":"Up", "accountcode":"",
                "caller":{"name":"", "number":""}, "connected":{"name":"", "number":""},
                "dialplan":{"context":"default", "exten":"s", "priority":1},
                "creationtime":"now", "language":"en"
            }))?;
            let (progress, received) = watch::channel(CallSnapshot {
                progress: OwnedCallProgress::new().observe(CallMilestone::Stasis),
                channel: Some(channel.clone()),
                failure: None,
                cleanup: Cleanup::Pending,
                media_probe_failure: None,
            });
            let (_lifetime, lifetime) = oneshot::channel();
            calls.entries.insert(
                channel.id.clone(),
                Entry {
                    progress,
                    lifetime,
                    work: Work::Idle,
                    probe: Probe::Finished,
                },
            );
            let mut overflowing = channel;
            overflowing.channelvars = Some(
                (0..129)
                    .map(|index| (format!("key-{index}"), String::new()))
                    .collect(),
            );
            let event = api::Event {
                application: "app".into(),
                timestamp: "now".into(),
                kind: if terminal {
                    api::EventKind::StasisEnd {
                        channel: overflowing,
                    }
                } else {
                    api::EventKind::ChannelStateChange {
                        channel: overflowing,
                    }
                },
            };
            assert!(matches!(calls.event(&event), Err(Error::Capacity)));
            calls.close().await?;
            let snapshot = received.borrow();
            assert_eq!(snapshot.cleanup, Cleanup::Complete);
            assert_eq!(
                snapshot.progress.completion(),
                Some(if terminal {
                    CallCompletion::StasisExited
                } else {
                    CallCompletion::Cancelled
                })
            );
            if terminal {
                assert!(received_deletions.try_recv().is_err());
            } else {
                received_deletions.recv().await.ok_or(Error::Closed)?;
            }
        }
        Ok::<(), Error>(())
    }
    .await;
    server.abort();
    let cleanup = match server.await {
        Err(error) if error.is_cancelled() => Ok(()),
        Err(error) => Err(Error::Cleanup(format!("server failed: {error}"))),
        Ok(result) => result.map_err(Error::from),
    };
    match (result, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(primary), Err(cleanup)) => Err(Error::Cleanup(format!(
            "{primary}; cleanup failed: {cleanup}"
        ))),
    }
}
