use super::{CallKey, ChannelEntry, Control, ControlError, Effect, Outcome};
use rusterisk_api::{Channel, ChannelId, ChannelState};
use rusterisk_core::{Call, DialFork, DialForkError, DialplanToken};
use std::collections::BTreeMap;

pub(super) struct DialBranches {
    pub token: DialplanToken,
    pub fork: DialFork,
    children: Vec<CallKey>,
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
