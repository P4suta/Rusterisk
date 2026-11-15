use super::{CallKey, ChannelEntry, Control, ControlError, Effect, Outcome};
use rusterisk_api::{Channel, ChannelId, ChannelState};
use rusterisk_core::{
    Call, CallState, DialFailure, DialFork, DialForkError, DialForkOutcome, DialForkState,
    DialplanToken,
};
use std::collections::{BTreeMap, BTreeSet};

pub(super) struct DialBranches {
    pub token: DialplanToken,
    pub fork: DialFork,
    children: Vec<CallKey>,
    failure: DialFailure,
    parent: CallKey,
    bridge: Option<u64>,
}

pub(super) struct DialBranch {
    parent: CallKey,
    index: u8,
    token: DialplanToken,
}

fn failure(cause: u16) -> DialFailure {
    match cause {
        16 | 19 => DialFailure::NoAnswer,
        17 => DialFailure::Busy,
        34 => DialFailure::Congestion,
        _ => DialFailure::Unavailable,
    }
}

fn failure_status(failure: DialFailure) -> &'static str {
    match failure {
        DialFailure::Unavailable => "CHANUNAVAIL",
        DialFailure::Congestion => "CONGESTION",
        DialFailure::Busy => "BUSY",
        DialFailure::NoAnswer => "NOANSWER",
    }
}

fn fork_error(error: DialForkError) -> ControlError {
    match error {
        DialForkError::Capacity => ControlError::Capacity,
        DialForkError::Clock => ControlError::Adapter("Dial timer overflow".into()),
        DialForkError::Branch | DialForkError::Stale => {
            ControlError::Conflict("Dial branch changed")
        }
    }
}

impl Control {
    pub(super) fn ring_dial_branch(&mut self, child: &CallKey) -> Result<Outcome, ControlError> {
        let mut outcome = Outcome::empty();
        let Some(branch) = self.dial_branch(child) else {
            return Ok(outcome);
        };
        let entry = self
            .channels
            .get_mut(&branch.parent.id)
            .filter(|entry| entry.call.generation() == branch.parent.generation)
            .ok_or(ControlError::NotFound("Channel not found"))?;
        let dialing = entry
            .dialplan
            .as_ref()
            .and_then(|frame| frame.dial.as_ref())
            .is_some_and(|dial| {
                dial.token == branch.token
                    && matches!(dial.fork.state(), DialForkState::Dialing { .. })
            });
        if dialing
            && matches!(
                entry.snapshot.state,
                ChannelState::Down | ChannelState::Ring
            )
        {
            entry.call = entry.call.apply(rusterisk_core::CallEvent::Ring).call();
            entry.snapshot.state = ChannelState::Ringing;
            outcome.effects.push(Effect::Ring { key: branch.parent });
        }
        Ok(outcome)
    }

    pub(super) fn termination_budget(&self, id: &ChannelId) -> usize {
        let Ok(key) = self.key(id) else {
            return 2;
        };
        let mut discovered = BTreeSet::new();
        discovered.insert(key.clone());
        let mut pending = Vec::with_capacity(self.channels.len());
        pending.push(key);
        let mut effects = 0;
        while let Some(key) = pending.pop() {
            let Some(entry) = self
                .channels
                .get(&key.id)
                .filter(|entry| entry.call.generation() == key.generation)
            else {
                continue;
            };
            let managed_bridge = entry
                .call
                .bridge()
                .is_some_and(|token| self.bridges.values().any(|bridge| bridge.token == token));
            effects += 1 + usize::from(managed_bridge);
            if let Some(dial) = entry
                .dialplan
                .as_ref()
                .and_then(|frame| frame.dial.as_ref())
            {
                effects += usize::from(dial.bridge.is_some());
                for child in &dial.children {
                    if self.key(&child.id).is_ok_and(|current| current == *child)
                        && discovered.insert(child.clone())
                    {
                        pending.push(child.clone());
                    }
                }
            }
            if let Some(branch) = self.dial_branch(&key) {
                let winner = self.channels.get(&branch.parent.id)
                    .and_then(|entry| entry.dialplan.as_ref())
                    .and_then(|frame| frame.dial.as_ref())
                    .is_some_and(|dial| matches!(dial.fork.state(), DialForkState::Bridged { winner } if winner == branch.index));
                if winner && discovered.insert(branch.parent.clone()) {
                    pending.push(branch.parent);
                }
            }
        }
        effects
    }

    pub(super) fn dial_branch(&self, child: &CallKey) -> Option<DialBranch> {
        self.channels.iter().find_map(|(id, entry)| {
            let dial = entry.dialplan.as_ref()?.dial.as_ref()?;
            let index = dial.children.iter().position(|key| key == child)?;
            Some(DialBranch {
                parent: CallKey {
                    id: id.clone(),
                    generation: entry.call.generation(),
                },
                index: u8::try_from(index).ok()?,
                token: dial.token,
            })
        })
    }

    pub(super) fn failed_dial_branch(
        &mut self,
        branch: DialBranch,
        cause: u16,
        now: u64,
        timestamp: &str,
    ) -> Result<Outcome, ControlError> {
        let dial = self
            .channels
            .get_mut(&branch.parent.id)
            .filter(|entry| entry.call.generation() == branch.parent.generation)
            .and_then(|entry| entry.dialplan.as_mut())
            .and_then(|frame| {
                if frame
                    .dial
                    .as_ref()
                    .is_some_and(|dial| dial.token == branch.token)
                {
                    frame.dial.take()
                } else {
                    None
                }
            });
        let Some(mut dial) = dial else {
            return Ok(Outcome::empty());
        };
        let expired = dial.fork.tick(now);
        let decision = if expired.outcome == DialForkOutcome::Continue {
            dial.failure = DialFailure::NoAnswer;
            Ok(expired)
        } else {
            dial.failure = dial.failure.combine(failure(cause));
            dial.fork
                .failed(branch.token, branch.index, now)
                .map_err(fork_error)
        };
        let result = match decision {
            Ok(decision) => self
                .release_dial(&dial, decision.release, timestamp)
                .map(|outcome| (decision.outcome, outcome)),
            Err(error) => Err(error),
        };
        let frame = self
            .channels
            .get_mut(&branch.parent.id)
            .and_then(|entry| entry.dialplan.as_mut())
            .ok_or(ControlError::Conflict("Dial parent disappeared"))?;
        match result {
            Ok((DialForkOutcome::Continue, outcome)) => {
                if let Err(error) = frame.complete_dial(dial.token) {
                    frame.dial = Some(dial);
                    return Err(error);
                }
                self.channels
                    .get_mut(&branch.parent.id)
                    .ok_or(ControlError::NotFound("Channel not found"))?
                    .variables
                    .insert("DIALSTATUS".into(), failure_status(dial.failure).into());
                Ok(outcome)
            }
            Ok((DialForkOutcome::Pending, outcome)) => {
                frame.dial = Some(dial);
                Ok(outcome)
            }
            Ok((DialForkOutcome::End, mut outcome)) => {
                frame.dial = Some(dial);
                outcome.append(self.end_at(&branch.parent.id, timestamp, 16, now)?);
                Ok(outcome)
            }
            Ok((DialForkOutcome::Connect { .. }, _)) => {
                frame.dial = Some(dial);
                Err(ControlError::Conflict("Failed branch cannot answer"))
            }
            Err(error) => {
                frame.dial = Some(dial);
                Err(error)
            }
        }
    }

    pub(super) fn answered_dial_branch(
        &mut self,
        branch: DialBranch,
        now: u64,
        timestamp: &str,
    ) -> Result<Outcome, ControlError> {
        let dial = self
            .channels
            .get_mut(&branch.parent.id)
            .filter(|entry| entry.call.generation() == branch.parent.generation)
            .and_then(|entry| entry.dialplan.as_mut())
            .and_then(|frame| {
                if frame
                    .dial
                    .as_ref()
                    .is_some_and(|dial| dial.token == branch.token)
                {
                    frame.dial.take()
                } else {
                    None
                }
            });
        let Some(mut dial) = dial else {
            return Ok(Outcome::empty());
        };
        let result = dial
            .fork
            .answer(branch.token, branch.index, now)
            .map_err(fork_error)
            .and_then(|decision| {
                self.release_dial(&dial, decision.release, timestamp)
                    .map(|outcome| (decision.outcome, outcome))
            });
        let frame = self
            .channels
            .get_mut(&branch.parent.id)
            .and_then(|entry| entry.dialplan.as_mut())
            .ok_or(ControlError::Conflict("Dial parent disappeared"))?;
        match result {
            Ok((DialForkOutcome::Pending, outcome)) => {
                frame.dial = Some(dial);
                Ok(outcome)
            }
            Ok((DialForkOutcome::Connect { .. }, mut outcome)) => {
                frame.dial = Some(dial);
                self.channels
                    .get_mut(&branch.parent.id)
                    .ok_or(ControlError::NotFound("Channel not found"))?
                    .variables
                    .insert("DIALSTATUS".into(), "ANSWER".into());
                outcome.append(self.connect_dial(&branch.parent)?);
                Ok(outcome)
            }
            Ok((DialForkOutcome::Continue, outcome)) => {
                if let Err(error) = frame.complete_dial(dial.token) {
                    frame.dial = Some(dial);
                    return Err(error);
                }
                self.channels
                    .get_mut(&branch.parent.id)
                    .ok_or(ControlError::NotFound("Channel not found"))?
                    .variables
                    .insert("DIALSTATUS".into(), "NOANSWER".into());
                Ok(outcome)
            }
            Ok((DialForkOutcome::End, _)) => {
                frame.dial = Some(dial);
                Err(ControlError::Conflict(
                    "Answer cannot end its dial operation",
                ))
            }
            Err(error) => {
                frame.dial = Some(dial);
                Err(error)
            }
        }
    }

    pub(super) fn connect_dial(&mut self, parent: &CallKey) -> Result<Outcome, ControlError> {
        let Some(entry) = self
            .channels
            .get(&parent.id)
            .filter(|entry| entry.call.generation() == parent.generation)
        else {
            return Ok(Outcome::empty());
        };
        let Some(dial) = entry
            .dialplan
            .as_ref()
            .and_then(|frame| frame.dial.as_ref())
        else {
            return Ok(Outcome::empty());
        };
        if dial.bridge.is_some() {
            return Ok(Outcome::empty());
        }
        let winner = match dial.fork.state() {
            DialForkState::Bridged { winner } => winner,
            DialForkState::Dialing { .. } | DialForkState::Ended => return Ok(Outcome::empty()),
        };
        let peer = dial
            .children
            .get(usize::from(winner))
            .ok_or(ControlError::Conflict("Dial winner is missing"))?
            .clone();
        let mut outcome = Outcome::empty();
        if entry.call.state() != CallState::Up {
            outcome.effects.push(Effect::Answer {
                key: parent.clone(),
            });
            return Ok(outcome);
        }
        let parent_call = entry.call;
        let parent_caller = entry.snapshot.caller.clone();
        let peer_entry = self
            .channels
            .get(&peer.id)
            .filter(|entry| entry.call.generation() == peer.generation)
            .ok_or(ControlError::Conflict("Dial peer disappeared"))?;
        let peer_call = peer_entry.call;
        let peer_caller = peer_entry.snapshot.caller.clone();
        let token = self.generation()?;
        let parent_call = parent_call.join(token)?;
        let peer_call = peer_call.join(token)?;
        let parent_entry = self
            .channels
            .get_mut(&parent.id)
            .ok_or(ControlError::NotFound("Channel not found"))?;
        parent_entry.call = parent_call;
        parent_entry.snapshot.connected = peer_caller;
        parent_entry
            .dialplan
            .as_mut()
            .and_then(|frame| frame.dial.as_mut())
            .ok_or(ControlError::Conflict("Dial operation disappeared"))?
            .bridge = Some(token);
        let peer_entry = self
            .channels
            .get_mut(&peer.id)
            .ok_or(ControlError::NotFound("Channel not found"))?;
        peer_entry.call = peer_call;
        peer_entry.snapshot.connected = parent_caller;
        outcome.effects.push(Effect::DialBridge {
            parent: parent.clone(),
            generation: token,
            peer: Some(peer),
        });
        Ok(outcome)
    }

    pub(super) fn begin_dial(
        &mut self,
        parent: &CallKey,
        token: DialplanToken,
        destinations: &str,
        timeout: Option<u16>,
        now: u64,
        timestamp: &str,
    ) -> Result<(DialBranches, Outcome), ControlError> {
        let destinations = destinations.split('&').collect::<Vec<_>>();
        if destinations.is_empty()
            || destinations.len() > 16
            || self.channels.len() + destinations.len() > self.capacity
        {
            return Err(ControlError::Capacity);
        }
        for destination in &destinations {
            let endpoint = destination
                .strip_prefix("PJSIP/")
                .ok_or(ControlError::Unsupported)?;
            ChannelId::try_from(endpoint.to_owned())?;
        }
        let count = u8::try_from(destinations.len()).map_err(|_| ControlError::Capacity)?;
        self.generation
            .checked_add(u64::from(count))
            .ok_or(ControlError::Capacity)?;
        let fork = DialFork::new(
            token,
            count,
            now,
            timeout.map(|seconds| u32::from(seconds) * 1000),
        )
        .map_err(fork_error)?;
        let parent_entry = self
            .channels
            .get(&parent.id)
            .ok_or(ControlError::NotFound("Channel not found"))?;
        let snapshot = parent_entry.snapshot.clone();
        let mut children = Vec::with_capacity(destinations.len());
        let mut planned = Vec::with_capacity(destinations.len());
        for endpoint in destinations {
            let mut selected = None;
            for _ in 0..=self.capacity {
                let generation = self.generation()?;
                let id = ChannelId::try_from(format!("dial-{generation:016x}"))?;
                if !self.channels.contains_key(&id) {
                    selected = Some(CallKey { id, generation });
                    break;
                }
            }
            planned.push((selected.ok_or(ControlError::Capacity)?, endpoint));
        }
        let mut outcome = Outcome::empty();
        for (key, endpoint) in planned {
            let id = key.id.clone();
            let generation = key.generation;
            let child = Channel {
                id: id.clone(),
                name: format!("{endpoint}-{generation:08x}"),
                state: ChannelState::Down,
                accountcode: snapshot.accountcode.clone(),
                caller: snapshot.caller.clone(),
                connected: snapshot.connected.clone(),
                dialplan: rusterisk_api::Dialplan {
                    context: snapshot.dialplan.context.clone(),
                    exten: snapshot.dialplan.exten.clone(),
                    priority: 1,
                    app_name: String::new(),
                    app_data: String::new(),
                },
                creationtime: timestamp.into(),
                language: snapshot.language.clone(),
            };
            self.channels.insert(
                id,
                ChannelEntry {
                    call: Call::outgoing(generation),
                    snapshot: child,
                    arguments: Vec::new(),
                    deadline: None,
                    variables: BTreeMap::new(),
                    dtmf: None,
                    dialplan: Some(super::dialplan::DialplanFrame::dormant(
                        self.dialplan_program.clone(),
                    )),
                    media_tasks: rusterisk_core::MediaTasks::new(generation),
                },
            );
            outcome.effects.push(Effect::Start {
                key: key.clone(),
                endpoint: endpoint.into(),
                caller: snapshot.caller.number.clone(),
                timeout: timeout.map(u32::from),
            });
            outcome.history.push(super::CallHistoryEvent::Started {
                key: key.clone(),
                timestamp: timestamp.into(),
            });
            children.push(key);
        }
        Ok((
            DialBranches {
                token,
                fork,
                children,
                failure: DialFailure::Unavailable,
                parent: parent.clone(),
                bridge: None,
            },
            outcome,
        ))
    }

    pub(super) fn release_dial(
        &mut self,
        dial: &DialBranches,
        release: u16,
        timestamp: &str,
    ) -> Result<Outcome, ControlError> {
        let mut outcome = Outcome::empty();
        for (index, key) in dial.children.iter().enumerate() {
            if release & (1 << index) != 0 && self.key(&key.id).is_ok_and(|current| current == *key)
            {
                outcome.append(self.end(&key.id, timestamp, 16)?);
            }
        }
        Ok(outcome)
    }

    pub(super) fn cancel_dial(
        &mut self,
        dial: &mut DialBranches,
        timestamp: &str,
    ) -> Result<Outcome, ControlError> {
        let release = dial.fork.cancel().release;
        let mut outcome = Outcome::empty();
        if let Some(generation) = dial.bridge.take() {
            outcome.effects.push(Effect::DialBridge {
                parent: dial.parent.clone(),
                generation,
                peer: None,
            });
        }
        outcome.append(self.release_dial(dial, release, timestamp)?);
        Ok(outcome)
    }
}
