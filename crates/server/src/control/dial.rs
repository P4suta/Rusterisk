use super::{CallKey, ChannelEntry, Control, ControlError, Effect, Outcome};
use rusterisk_api::{Channel, ChannelId, ChannelState};
use rusterisk_core::{Call, DialFailure, DialFork, DialForkError, DialForkOutcome, DialplanToken};
use std::collections::BTreeMap;

pub(super) struct DialBranches {
    pub token: DialplanToken,
    pub fork: DialFork,
    children: Vec<CallKey>,
    failure: DialFailure,
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
                },
            );
            outcome.effects.push(Effect::Start {
                key: key.clone(),
                endpoint: endpoint.into(),
                caller: snapshot.caller.number.clone(),
                timeout: timeout.map(u32::from),
            });
            children.push(key);
        }
        Ok((
            DialBranches {
                token,
                fork,
                children,
                failure: DialFailure::Unavailable,
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
        self.release_dial(dial, release, timestamp)
    }
}
