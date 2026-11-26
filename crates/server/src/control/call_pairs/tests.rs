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

#[test]
fn unrelated_generation_changes_cannot_assign_two_live_calls_the_same_slot(
) -> Result<(), ControlError> {
    let mut control = Control::new(64);
    let first = participant(&mut control, "first-slot", 1)?;
    for _ in 0..63 {
        control.generation()?;
    }
    let second = participant(&mut control, "second-slot", 2)?;
    assert_ne!(first.handle().slot(), second.handle().slot());
    assert!(control.has_channel(&first));
    assert!(control.has_channel(&second));
    Ok(())
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
fn admitting_a_phone_uses_the_same_memory_with_one_or_4096_dialplan_steps(
) -> Result<(), ControlError> {
    let measured = |count| -> Result<_, ControlError> {
        let program = rusterisk_config::Dialplan {
            steps: (0..count)
                .map(|number| rusterisk_config::Step {
                    context: "internal".into(),
                    extension: format!("{number:04}"),
                    priority: 1,
                    application: rusterisk_config::Application::Hangup,
                })
                .collect(),
        };
        let mut control = Control::with_dialplan(2, program);
        let mut result = Ok(());
        let allocation = allocation_counter::measure(|| {
            result = participant(&mut control, "phone", 1).map(|_| ());
        });
        result?;
        Ok((allocation.count_total, allocation.bytes_total))
    };
    let small = measured(1)?;
    let large = measured(4096)?;
    assert_eq!(large, small);
    Ok(())
}

#[test]
fn a_retiring_call_counts_towards_the_configured_capacity() -> Result<(), ControlError> {
    let mut control = Control::new(1);
    let ended = participant(&mut control, "retiring", 1)?;
    assert_eq!(control.call_slots().active, 1);
    assert_eq!(control.call_slots().awaiting_release, 0);
    assert_eq!(control.call_slots().capacity_refusals, 0);
    end(&mut control, ended.clone())?;
    let mut other = ended.clone();
    other.id = ChannelId::try_from("another-call".to_owned())?;
    assert!(!control.media_released(&other));
    assert_eq!(control.call_slots().active, 0);
    assert_eq!(control.call_slots().awaiting_release, 1);
    assert!(matches!(
        participant(&mut control, "replacement", 1),
        Err(ControlError::Capacity)
    ));
    assert_eq!(control.call_slots().capacity_refusals, 1);
    assert!(control.media_released(&ended));
    participant(&mut control, "replacement", 1)?;
    assert_eq!(control.call_slots().awaiting_release, 0);
    assert_eq!(control.call_slots().active, 1);
    assert_eq!(control.call_slots().capacity_refusals, 1);
    Ok(())
}

#[test]
fn a_full_table_waits_for_media_to_release_the_ended_call() -> Result<(), ControlError> {
    let mut control = Control::new(64);
    let ended = participant(&mut control, "retiring", 1)?;
    for number in 1..64 {
        participant(&mut control, &format!("other-{number}"), 1)?;
    }
    end(&mut control, ended.clone())?;
    assert!(matches!(
        participant(&mut control, "replacement", 1),
        Err(ControlError::Capacity)
    ));
    assert!(control.media_released(&ended));
    let replacement = participant(&mut control, "replacement", 1)?;
    assert_eq!(replacement.handle().slot(), ended.handle().slot());
    assert_ne!(replacement.generation, ended.generation);
    assert!(!control.media_released(&ended));
    assert!(control.has_channel(&replacement));
    Ok(())
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
    let (mut entry, mut cleanup) = control
        .channels
        .end(&members[1].id)
        .ok_or(ControlError::NotFound("Missing reused channel"))?;
    for _step in cleanup.by_ref() {}
    assert!(control.channels.await_media(cleanup));
    assert!(control.media_released(&members[1]));
    let replacement = control.call_generation(0)?;
    entry.call = Call::incoming(replacement)
        .apply(CallEvent::Answer)
        .call()
        .join(200)?;
    let current = entry.call;
    control.channels.insert(members[1].id.clone(), entry)?;
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
