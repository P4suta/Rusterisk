use super::{
    adopt, api, originate, CallCompletion, CallSnapshot, Calls, Cleanup, Client, Command,
    CreateRequest, Entry, Error, Lifetime, Originate, OwnedCall, OwnedCallProgress, Probe,
    Requests, Start, Work,
};
use std::{
    future::Future,
    pin::pin,
    task::{Context, Waker},
};
use tokio::sync::{oneshot, watch};

fn driver() -> Result<(Requests, Calls), Error> {
    let client = Client::new("http://127.0.0.1:9", "app", "password")?;
    Ok(Calls::new(
        client,
        api::ChannelId::try_from("app".to_owned())?,
    ))
}

fn channel(id: &str) -> Result<api::Channel, Error> {
    Ok(serde_json::from_value(
        serde_json::json!({"id":id,"name":"PJSIP/phone","state":"Up","accountcode":"","caller":{"name":"","number":"100"},"connected":{"name":"","number":"200"},"dialplan":{"context":"internal","exten":"200","priority":1},"creationtime":"now","language":"en"}),
    )?)
}

fn pending() -> CallSnapshot {
    CallSnapshot {
        progress: OwnedCallProgress::new(),
        channel: None,
        failure: None,
        cleanup: Cleanup::Pending,
        media_probe_failure: None,
    }
}

fn owned(requests: &Requests, calls: &mut Calls, id: &str) -> Result<OwnedCall, Error> {
    let channel = channel(id)?;
    let id = channel.id.clone();
    let (progress, received) = watch::channel(CallSnapshot {
        channel: Some(channel),
        ..pending()
    });
    let (lifetime, dropped) = oneshot::channel();
    calls.entries.insert(
        id.clone(),
        Entry {
            progress,
            lifetime: dropped,
            work: Work::Idle,
            probe: Probe::Waiting,
        },
    );
    Ok(OwnedCall {
        id,
        progress: received,
        lifetime: Lifetime::Owned(lifetime),
        closure: requests.closures.notice(),
    })
}

fn set(calls: &mut Calls) -> bool {
    calls.closed.try_recv().is_ok()
}

#[tokio::test]
async fn a_dropped_call_sets_the_latch_and_the_next_settle_starts_its_cleanup() -> Result<(), Error>
{
    let (requests, mut calls) = driver()?;
    let call = owned(&requests, &mut calls, "dropped")?;
    calls.settle();
    assert!(calls.cleanups.is_empty());
    assert!(!set(&mut calls));
    drop(call);
    assert!(set(&mut calls));
    assert!(!set(&mut calls));
    calls.settle();
    assert_eq!(calls.cleanups.len(), 1);
    Ok(())
}

#[tokio::test]
async fn a_cancelled_call_sets_the_latch_and_its_later_drop_frees_its_entry() -> Result<(), Error> {
    let (requests, mut calls) = driver()?;
    let mut call = owned(&requests, &mut calls, "cancelled")?;
    call.cancel();
    assert!(set(&mut calls));
    calls.settle();
    assert_eq!(calls.cleanups.len(), 1);
    calls.cleaned(call.id().clone(), Ok(()));
    assert_eq!(calls.entries.len(), 1);
    assert!(!set(&mut calls));
    drop(call);
    assert!(set(&mut calls));
    calls.settle();
    assert!(calls.entries.is_empty());
    Ok(())
}

#[tokio::test]
async fn an_abandoned_start_sets_the_latch_after_the_session_accepts_it() -> Result<(), Error> {
    let (requests, mut calls) = driver()?;
    {
        let mut starting = pin!(originate(&requests, Originate::new("PJSIP/phone".into())?));
        assert!(starting
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
            .is_pending());
        let command = calls.commands.try_recv().map_err(|_| Error::Closed)?;
        calls.submit(command);
        assert_eq!(calls.entries.len(), 1);
        assert!(!set(&mut calls));
    }
    assert!(set(&mut calls));
    calls.settle();
    assert!(calls.entries.values().all(|entry| {
        entry.progress.borrow().progress.completion() == Some(CallCompletion::Cancelled)
    }));
    Ok(())
}

#[tokio::test]
async fn an_abandoned_adoption_sets_the_latch_after_the_session_accepts_it() -> Result<(), Error> {
    let (requests, mut calls) = driver()?;
    let incoming = channel("incoming")?;
    calls.event(&api::Event {
        application: "app".into(),
        timestamp: "now".into(),
        kind: api::EventKind::StasisStart {
            channel: incoming.clone(),
            args: Vec::new(),
        },
    })?;
    {
        let mut adopting = pin!(adopt(&requests, incoming.id.clone()));
        assert!(adopting
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
            .is_pending());
        let command = calls.commands.try_recv().map_err(|_| Error::Closed)?;
        calls.submit(command);
        assert_eq!(calls.entries.len(), 1);
        assert!(!set(&mut calls));
    }
    assert!(set(&mut calls));
    calls.settle();
    assert_eq!(calls.cleanups.len(), 1);
    Ok(())
}

#[tokio::test]
async fn a_released_slot_admits_the_next_start_before_another_settle() -> Result<(), Error> {
    let (requests, mut calls) = driver()?;
    let mut held = Vec::new();
    for index in 0..64 {
        held.push(owned(&requests, &mut calls, &format!("held-{index}"))?);
    }
    let released = held.pop().ok_or(Error::Closed)?;
    if let Some(entry) = calls.entries.get_mut(released.id()) {
        entry.work = Work::Done;
    }
    drop(released);
    let (progress, _received) = watch::channel(pending());
    let (_lifetime, dropped) = oneshot::channel();
    let (reply, mut response) = oneshot::channel();
    calls.submit(Command::Create(CreateRequest {
        id: api::ChannelId::try_from("next".to_owned())?,
        options: Start::Originate(Originate::new("PJSIP/phone".into())?),
        progress,
        lifetime: dropped,
        reply,
    }));
    assert!(matches!(response.try_recv(), Ok(Ok(()))));
    assert_eq!(calls.entries.len(), 64);
    Ok(())
}
