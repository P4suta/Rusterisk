use super::{Controller, Credentials, Request, Runner};
use crate::{
    Action, CallKey, ControlError, Effect, IncomingCall, IncomingTarget, Milestone, Outcome, Signal,
};
use rusterisk_api::ChannelId;
use rusterisk_config::Config;
use rusterisk_sip::{DialogRefer, Engine, Outbound, ReferFailure, ReferProgress, SipEvent};
use std::collections::BTreeSet;
use tokio::sync::oneshot;

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}

fn id(value: &str) -> Result<ChannelId, ControlError> {
    Ok(ChannelId::try_from(value.to_owned())?)
}

fn configuration() -> Result<Config, ControlError> {
    Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=password\n",
        "[udp]\ntype=transport\nprotocol=udp\nbind=127.0.0.1:5060\n[credential]\ntype=auth\nusername=phone\npassword=password\n[alice-aor]\ntype=aor\ncontact=sip:alice@127.0.0.1:5061\n[bob-aor]\ntype=aor\ncontact=sip:bob@127.0.0.1:5062\n[alice]\ntype=endpoint\ntransport=udp\nauth=credential\naors=alice-aor\ncontext=internal\nallow=ulaw\n[bob]\ntype=endpoint\ntransport=udp\nauth=credential\naors=bob-aor\ncontext=internal\nallow=ulaw\n",
        "",
        "[internal]\nexten => 100,1,Dial(PJSIP/bob,60)\nexten => 300,1,Stasis(calls)\n",
    )
    .map_err(failure)
}

fn refer(config: &Config, source: &CallKey) -> Result<DialogRefer, ControlError> {
    use rsipstack::sip::{HeadersExt, SipMessage};
    let mut engine = Engine::new(config.clone(), [7; 32]);
    let started = engine
        .originate(
            Outbound {
                id: source.id.as_str().into(),
                generation: source.generation,
                endpoint: "alice".into(),
                caller: "100".into(),
                local_rtp: "127.0.0.1:10000".parse().map_err(failure)?,
                timeout_ms: Some(60_000),
            },
            0,
        )
        .map_err(failure)?;
    let request = match SipMessage::try_from(
        started
            .datagrams
            .first()
            .ok_or(ControlError::Invalid("Missing outgoing INVITE"))?
            .bytes
            .as_slice(),
    )
    .map_err(failure)?
    {
        SipMessage::Request(request) => request,
        SipMessage::Response(_) => return Err(ControlError::Invalid("Expected outgoing INVITE")),
    };
    let via = request.via_header().map_err(failure)?.value();
    let from = request.from_header().map_err(failure)?.value();
    let to = format!(
        "{};tag=phone",
        request.to_header().map_err(failure)?.value()
    );
    let call_id = request.call_id_header().map_err(failure)?.value();
    let cseq = request.cseq_header().map_err(failure)?.value();
    let body = "v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=call\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\nm=audio 10002 RTP/AVP 0\r\na=sendrecv\r\n";
    let answered = format!(
        "SIP/2.0 200 OK\r\nVia: {via}\r\nFrom: {from}\r\nTo: {to}\r\nCall-ID: {call_id}\r\nCSeq: {cseq}\r\nContact: <sip:alice@127.0.0.1:5061>\r\nContent-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    let peer = "127.0.0.1:5061".parse().map_err(failure)?;
    let answered = engine
        .receive(answered.as_bytes(), peer, 1)
        .map_err(failure)?;
    assert!(answered.events.iter().any(|event| matches!(event, SipEvent::Answered { id, generation, .. } if id == source.id.as_str() && *generation == source.generation)));
    let packet = format!(
        "REFER sip:100@127.0.0.1:5060 SIP/2.0\r\nVia: SIP/2.0/UDP 127.0.0.1:5061;branch=z9hG4bK-refer;rport\r\nFrom: {to}\r\nTo: {from}\r\nCall-ID: {call_id}\r\nCSeq: 100 REFER\r\nContact: <sip:alice@127.0.0.1:5061>\r\nRefer-To: <sip:300@outside.invalid>\r\nMax-Forwards: 70\r\nContent-Length: 0\r\n\r\n"
    );
    let requested = engine
        .receive(packet.as_bytes(), peer, 2)
        .map_err(failure)?;
    requested
        .events
        .into_iter()
        .find_map(|event| {
            if let SipEvent::BlindTransferRequested {
                id,
                generation,
                refer,
            } = event
            {
                (id == source.id.as_str() && generation == source.generation).then_some(refer)
            } else {
                None
            }
        })
        .ok_or(ControlError::Invalid("Missing outgoing transfer request"))
}

fn prepare(runner: &mut Runner, index: usize) -> Result<CallKey, ControlError> {
    let channel = id(&format!("source-{index}"))?;
    runner.control.incoming(
        IncomingCall {
            id: channel.clone(),
            endpoint: "alice".into(),
            caller: "100".into(),
            context: "internal".into(),
            extension: "100".into(),
            target: IncomingTarget::Dialplan,
        },
        "incoming",
    )?;
    let source = runner.control.key(&channel)?;
    let started =
        runner
            .control
            .start_dialplan(&source, runner.config.dialplan.clone(), 0, "start")?;
    let [Effect::Start { key: survivor, .. }] = started.effects.as_slice() else {
        return Err(ControlError::Invalid("Expected one Dial child"));
    };
    let survivor = survivor.clone();
    for key in [&survivor, &source] {
        runner.control.signal_at(
            Signal::Milestone {
                key: key.clone(),
                milestone: Milestone::Answered,
            },
            "answered",
            1,
        )?;
    }
    let reference = refer(&runner.config, &source)?;
    let transferred = runner.control.signal_at(
        Signal::BlindTransferRequested {
            key: source.clone(),
            refer: reference,
        },
        "transfer",
        2,
    )?;
    assert!(transferred.effects.iter().any(|effect| matches!(
        effect,
        Effect::ReferProgress {
            progress: ReferProgress::HandedOff,
            ..
        }
    )));
    assert!(runner.control.channel(&source.id).is_err());
    let bridge = id(&format!("bridge-{index}"))?;
    runner.control.apply(
        Action::CreateBridge {
            id: bridge.clone(),
            name: String::new(),
        },
        "bridge",
        3,
    )?;
    runner.control.apply(
        Action::AddChannels {
            bridge,
            channels: [survivor.id.clone()].into_iter().collect(),
        },
        "bridge",
        3,
    )?;
    Ok(survivor)
}

fn apply(runner: &mut Runner, action: Action) -> Result<Outcome, ControlError> {
    let (reply, mut response) = oneshot::channel();
    runner.request(
        Request::Apply {
            credentials: Credentials::new("app".into(), "password".into())?,
            origin: None,
            action,
            variables: Default::default(),
            reply,
        },
        4,
    )?;
    response.try_recv().map_err(failure)?
}

fn fill(runner: &mut Runner, key: &CallKey, count: usize) -> Result<(), ControlError> {
    for index in 0..count {
        apply(
            runner,
            if index % 2 == 0 {
                Action::Mute {
                    channel: key.id.clone(),
                    direction: rusterisk_core::MuteDirection::Both,
                }
            } else {
                Action::Unmute {
                    channel: key.id.clone(),
                    direction: rusterisk_core::MuteDirection::Both,
                }
            },
        )?;
    }
    Ok(())
}

#[tokio::test]
async fn a_transfer_survivor_reserves_its_final_notification_before_hangup(
) -> Result<(), ControlError> {
    let (_controller, mut runner, mut effects) = Controller::new(configuration()?, 64);
    runner.control.subscribe("calls")?;
    let survivor = prepare(&mut runner, 0)?;
    fill(&mut runner, &survivor, 126)?;
    assert_eq!(runner.effects.capacity(), 2);
    assert!(matches!(
        apply(&mut runner, Action::Hangup(survivor.id.clone())),
        Err(ControlError::Capacity)
    ));
    assert!(runner.control.channel(&survivor.id).is_ok());
    assert_eq!(runner.control.terminal_effect_reserve(), 3);
    assert!(matches!(
        effects.try_recv().map_err(failure)?,
        Effect::Gate { .. }
    ));
    apply(&mut runner, Action::Hangup(survivor.id.clone()))?;
    assert!(runner.control.channel(&survivor.id).is_err());
    for _ in 0..125 {
        assert!(matches!(
            effects.try_recv().map_err(failure)?,
            Effect::Gate { .. }
        ));
    }
    assert!(matches!(
        effects.try_recv().map_err(failure)?,
        Effect::ReferProgress {
            progress: ReferProgress::Failed(ReferFailure::Cancelled),
            ..
        }
    ));
    assert!(
        matches!(effects.try_recv().map_err(failure)?, Effect::Bridge { channels, .. } if channels.is_empty())
    );
    assert!(
        matches!(effects.try_recv().map_err(failure)?, Effect::Release { key } if key == survivor)
    );
    assert!(effects.is_empty());
    assert_eq!(runner.control.terminal_effect_reserve(), 0);
    runner.close().await
}

#[tokio::test]
async fn shutdown_keeps_every_transfer_final_and_release_with_a_full_effect_channel(
) -> Result<(), ControlError> {
    let config = configuration()?;
    let (_controller, mut runner, mut effects) = Controller::new(config, 64);
    runner.control.subscribe("calls")?;
    let mut survivors = BTreeSet::new();
    for index in 0..63 {
        survivors.insert(prepare(&mut runner, index)?);
    }
    assert_eq!(runner.control.terminal_effect_reserve(), 63 * 3);
    let survivor = survivors
        .first()
        .ok_or(ControlError::Invalid("Missing transfer survivor"))?
        .clone();
    fill(&mut runner, &survivor, 128)?;
    assert_eq!(effects.len(), 128);
    assert!(runner.pending_effects.is_empty());
    let (closed, drain) = {
        let mut close = std::pin::pin!(runner.close());
        assert!(futures_util::poll!(close.as_mut()).is_pending());
        let drain = tokio::spawn(async move {
            let mut received = Vec::new();
            while let Some(effect) = effects.recv().await {
                received.push(effect);
            }
            received
        });
        (close.await, drain)
    };
    drop(runner);
    let received = drain.await.map_err(failure)?;
    assert!(
        closed.is_ok(),
        "shutdown result={closed:?}; drained effects={}",
        received.len()
    );
    let mut released = BTreeSet::new();
    let mut finished = BTreeSet::new();
    let mut bridges = BTreeSet::new();
    let mut gates = 0;
    for effect in received {
        match effect {
            Effect::Release { key } => assert!(released.insert(key)),
            Effect::ReferProgress {
                token,
                progress: ReferProgress::Failed(ReferFailure::Cancelled),
                ..
            } => assert!(finished.insert(token)),
            Effect::Bridge { id, channels, .. } => {
                assert!(channels.is_empty());
                assert!(bridges.insert(id));
            }
            Effect::Gate { key, .. } => {
                assert_eq!(key, survivor);
                gates += 1;
            }
            Effect::ReferProgress { .. }
            | Effect::RejectRefer { .. }
            | Effect::ControlRecording { .. }
            | Effect::ReserveWebSocket { .. }
            | Effect::EndRecording { .. }
            | Effect::PlayMedia { .. }
            | Effect::StartMusic { .. }
            | Effect::MusicClassUnavailable { .. }
            | Effect::ControlPlayback { .. }
            | Effect::Playback { .. }
            | Effect::Dtmf { .. }
            | Effect::PeerDirection { .. }
            | Effect::Answer { .. }
            | Effect::Ring { .. }
            | Effect::Start { .. }
            | Effect::ReleaseBridge { .. }
            | Effect::DialBridge { .. }
            | Effect::RecordMedia { .. }
            | Effect::ConnectExternalRtp { .. }
            | Effect::ConnectWebSocket { .. }
            | Effect::ConnectAudioSocket { .. } => {
                return Err(ControlError::Invalid("Unexpected transfer cleanup effect"));
            }
        }
    }
    assert_eq!(released, survivors);
    assert_eq!((finished.len(), bridges.len(), gates), (63, 63, 128));
    Ok(())
}
