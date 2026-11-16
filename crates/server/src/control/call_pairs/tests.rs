use super::ActivePair;
use crate::control::{
    CallKey, Control, ControlError, Effect, IncomingCall, IncomingTarget, Milestone, Outcome,
    Signal,
};
use rusterisk_api::{ChannelId, ChannelState};
use rusterisk_core::{Call, CallEvent, OwnedCallPair};

fn participant(control: &mut Control, name: &str, bridge: u64) -> Result<CallKey, ControlError> {
    let id = ChannelId::try_from(name.to_owned())?;
    control.incoming(
        IncomingCall {
            id: id.clone(),
            endpoint: "phone".into(),
            caller: "100".into(),
            context: "internal".into(),
            extension: "100".into(),
            target: IncomingTarget::Dialplan,
        },
        "incoming",
    )?;
    let key = control.key(&id)?;
    control.signal_at(
        Signal::Milestone {
            key: key.clone(),
            milestone: Milestone::Answered,
        },
        "answer",
        0,
    )?;
    let entry = control
        .channels
        .get_mut(&id)
        .ok_or(ControlError::NotFound("Missing paired channel"))?;
    entry.call = entry.call.join(bridge)?;
    Ok(key)
}

fn pair(control: &mut Control, bridge: u64) -> Result<[CallKey; 2], ControlError> {
    let members = [
        participant(control, &format!("left-{bridge}"), bridge)?,
        participant(control, &format!("right-{bridge}"), bridge)?,
    ];
    let [left, right] = members.each_ref().map(|key| {
        control
            .channels
            .get(&key.id)
            .ok_or(ControlError::NotFound("Missing pair participant"))
            .map(|entry| entry.call)
    });
    let state = OwnedCallPair::new(left?, right?)
        .map_err(|error| ControlError::Adapter(format!("{error:?}")))?;
    assert!(control
        .call_pairs
        .insert(
            bridge,
            ActivePair {
                state,
                members: members.clone(),
            },
        )
        .is_none());
    Ok(members)
}

fn releases(outcome: &Outcome, key: &CallKey) -> usize {
    outcome
        .effects
        .iter()
        .filter(|effect| matches!(effect, Effect::Release { key: released } if released == key))
        .count()
}

fn end(control: &mut Control, key: CallKey) -> Result<Outcome, ControlError> {
    control.signal_at(Signal::Terminated { key, cause: 16 }, "peer end", 1)
}

#[test]
fn moving_a_pair_member_preserves_the_other_current_call_and_bridge() -> Result<(), ControlError> {
    for moved in 0..2 {
        let mut control = Control::new(4);
        let members = pair(&mut control, 100)?;
        let entry = control
            .channels
            .get_mut(&members[moved].id)
            .ok_or(ControlError::NotFound("Missing moved participant"))?;
        entry.call = entry.call.leave(100)?.join(200)?;
        let outcome = end(&mut control, members[0].clone())?;
        assert_eq!(releases(&outcome, &members[0]), 1);
        assert_eq!(releases(&outcome, &members[1]), 0);
        assert!(
            matches!(outcome.effects.as_slice(), [Effect::ReleaseBridge { generation: 100 }, Effect::Release { key }] if key == &members[0])
        );
        assert_eq!(control.channel(&members[1].id)?.state, ChannelState::Up);
        assert_eq!(
            control
                .channels
                .get(&members[1].id)
                .ok_or(ControlError::NotFound("Missing retained participant"))?
                .call
                .bridge(),
            if moved == 1 { Some(200) } else { None }
        );
        assert!(control.call_pairs.is_empty());
        let repeated = end(&mut control, members[0].clone())?;
        assert!(
            matches!(repeated.effects.as_slice(), [Effect::Release { key }] if key == &members[0])
        );
        let stopped = control.shutdown("stop")?;
        assert_eq!(releases(&stopped, &members[1]), 1);
    }
    Ok(())
}

#[test]
fn a_reused_peer_identifier_cannot_end_the_replacement_generation() -> Result<(), ControlError> {
    let mut control = Control::new(4);
    let members = pair(&mut control, 100)?;
    let replacement = control.generation()?;
    let entry = control
        .channels
        .get_mut(&members[1].id)
        .ok_or(ControlError::NotFound("Missing reused channel"))?;
    entry.call = Call::incoming(replacement)
        .apply(CallEvent::Answer)
        .call()
        .join(200)?;
    let current = entry.call;
    let outcome = end(&mut control, members[0].clone())?;
    assert_eq!(releases(&outcome, &members[0]), 1);
    assert_eq!(releases(&outcome, &members[1]), 0);
    assert!(
        matches!(outcome.effects.as_slice(), [Effect::ReleaseBridge { generation: 100 }, Effect::Release { key }] if key == &members[0])
    );
    assert_eq!(
        control
            .channels
            .get(&members[1].id)
            .ok_or(ControlError::NotFound("Missing replacement generation"))?
            .call,
        current
    );
    assert!(control.call_pairs.is_empty());
    control.shutdown("stop")?;
    Ok(())
}

#[test]
fn an_old_generation_does_not_release_the_current_pair_owner() -> Result<(), ControlError> {
    let mut control = Control::new(4);
    let members = pair(&mut control, 100)?;
    let mut stale = members[0].clone();
    stale.generation = stale
        .generation
        .checked_add(100)
        .ok_or(ControlError::Capacity)?;
    let outcome = end(&mut control, stale.clone())?;
    assert!(matches!(outcome.effects.as_slice(), [Effect::Release { key }] if key == &stale));
    assert_eq!(control.call_pairs.len(), 1);
    for member in &members {
        assert_eq!(control.key(&member.id)?, *member);
        assert_eq!(control.channel(&member.id)?.state, ChannelState::Up);
    }
    control.shutdown("stop")?;
    Ok(())
}

#[test]
fn all_live_pair_owners_fit_the_terminal_reserve_and_release_once() -> Result<(), ControlError> {
    let mut control = Control::new(64);
    let mut members = Vec::with_capacity(64);
    for bridge in 100..132 {
        members.extend(pair(&mut control, bridge)?);
    }
    assert_eq!(control.channels.len(), 64);
    assert_eq!(control.call_pairs.len(), control.capacity / 2);
    let reserve = control.terminal_effect_reserve();
    assert_eq!(reserve, 128);
    for member in &members {
        assert_eq!(control.termination_budget(&member.id), 4);
    }
    let outcome = control.shutdown("stop")?;
    assert_eq!(outcome.effects.len(), 96);
    assert!(outcome.effects.len() <= reserve);
    for member in &members {
        assert_eq!(releases(&outcome, member), 1);
    }
    for bridge in 100..132 {
        assert_eq!(outcome.effects.iter().filter(|effect| matches!(effect, Effect::ReleaseBridge { generation } if *generation == bridge)).count(), 1);
    }
    assert!(control.channels.is_empty());
    assert!(control.call_pairs.is_empty());
    assert!(control.shutdown("duplicate stop")?.effects.is_empty());
    Ok(())
}
