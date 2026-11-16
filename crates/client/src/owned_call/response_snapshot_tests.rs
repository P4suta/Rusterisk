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
