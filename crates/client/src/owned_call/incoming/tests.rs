use super::super::{
    api, AdoptRequest, CallSnapshot, Calls, Cleanup, Client, Command, Error, OwnedCallProgress,
};
use tokio::sync::{oneshot, watch};

fn driver() -> Result<Calls, Error> {
    let client = Client::new("http://127.0.0.1:9", "app", "password")?;
    Ok(Calls::new(client, api::ChannelId::try_from("app".to_owned())?).1)
}

fn channel(id: &str) -> Result<api::Channel, Error> {
    Ok(serde_json::from_value(
        serde_json::json!({"id":id,"name":"PJSIP/phone","state":"Ringing","accountcode":"","caller":{"name":"","number":"100"},"connected":{"name":"","number":"200"},"dialplan":{"context":"internal","exten":"200","priority":1},"creationtime":"now","language":"en"}),
    )?)
}

fn entered(channel: api::Channel) -> api::Event {
    api::Event {
        application: "app".into(),
        timestamp: "now".into(),
        kind: api::EventKind::StasisStart {
            channel,
            args: Vec::new(),
        },
    }
}

#[test]
fn observations_bound_live_channels_and_release_capacity_when_stasis_ends() -> Result<(), Error> {
    let mut calls = driver()?;
    for index in 0..64 {
        calls.event(&entered(channel(&format!("incoming-{index}"))?))?;
    }
    calls.event(&entered(channel("incoming-0")?))?;
    assert_eq!(calls.incoming.len(), 64);
    assert!(matches!(
        calls.event(&entered(channel("overflow")?)),
        Err(Error::Capacity)
    ));
    calls.event(&api::Event {
        application: "app".into(),
        timestamp: "later".into(),
        kind: api::EventKind::StasisEnd {
            channel: channel("incoming-0")?,
        },
    })?;
    calls.event(&entered(channel("replacement")?))?;
    assert_eq!(calls.incoming.len(), 64);
    let mut foreign = entered(channel("foreign")?);
    foreign.application = "other".into();
    calls.event(&foreign)?;
    assert!(!calls
        .incoming
        .contains_key(&api::ChannelId::try_from("foreign".to_owned())?));
    Ok(())
}

#[test]
fn abandoned_adoption_does_not_claim_or_cancel_an_observed_channel() -> Result<(), Error> {
    let mut calls = driver()?;
    let event = entered(channel("abandoned")?);
    calls.event(&event)?;
    let id = api::ChannelId::try_from("abandoned".to_owned())?;
    let (progress, _snapshot) = watch::channel(CallSnapshot {
        progress: OwnedCallProgress::new(),
        channel: None,
        failure: None,
        cleanup: Cleanup::Pending,
        media_probe_failure: None,
    });
    let (_lifetime, lifetime) = oneshot::channel();
    let (reply, response) = oneshot::channel();
    drop(response);
    calls.submit(Command::Adopt(AdoptRequest {
        id: id.clone(),
        progress,
        lifetime,
        reply,
    }));
    assert!(calls.incoming.contains_key(&id));
    assert!(!calls.used.contains(&id));
    assert!(calls.entries.is_empty());
    assert!(calls.cleanups.is_empty());
    Ok(())
}

#[test]
fn retired_identifiers_cannot_repopulate_the_incoming_observation_cache() -> Result<(), Error> {
    let mut calls = driver()?;
    let channel = channel("retired")?;
    calls.used.insert(channel.id.clone());
    calls.event(&entered(channel))?;
    assert!(calls.incoming.is_empty());
    Ok(())
}
