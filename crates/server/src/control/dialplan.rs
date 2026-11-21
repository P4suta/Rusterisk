use super::{CallKey, Control, ControlError, Effect, Outcome, PlaybackCompletion, VariableAccess};
use rusterisk_api::{ChannelId, ChannelState, EventKind};
use rusterisk_config::{expand_variables, Application, Dialplan, ExpansionError};
use rusterisk_core::{
    AnswerProgress, AnswerStatus, CallState, DialFailure, DialForkOutcome, DialplanContinuation,
    DialplanDispatch, DialplanError, DialplanInstruction, DialplanMachine, DialplanState,
    DialplanToken, MediaTaskKind, MediaTaskToken, StasisMembership,
};

pub(super) struct DialplanFrame {
    program: Dialplan,
    extension: String,
    machine: Option<DialplanMachine>,
    answer: Option<AnswerProgress>,
    pub(super) dial: Option<super::dial::DialBranches>,
    playback: Option<PlaybackWait>,
}

enum PlaybackWait {
    Answering {
        token: DialplanToken,
        name: String,
    },
    Playing {
        token: DialplanToken,
        media: MediaTaskToken,
    },
}

impl DialplanFrame {
    pub(super) fn program(&self) -> &Dialplan {
        &self.program
    }
    pub(super) fn complete_dial(&mut self, token: DialplanToken) -> Result<(), ControlError> {
        if self
            .machine
            .as_mut()
            .is_some_and(|machine| machine.complete(token, DialplanContinuation::Advance))
        {
            Ok(())
        } else {
            Err(ControlError::Conflict("Dial completion changed"))
        }
    }

    pub(super) fn dormant(program: Dialplan) -> Self {
        Self {
            program,
            extension: String::new(),
            machine: None,
            answer: None,
            dial: None,
            playback: None,
        }
    }
}

fn core_error(error: DialplanError) -> ControlError {
    match error {
        DialplanError::Program => ControlError::Invalid("Invalid dialplan position"),
        DialplanError::Clock => ControlError::Adapter("Dialplan timer overflow".into()),
        DialplanError::Capacity => ControlError::Capacity,
    }
}

impl Control {
    pub(super) fn finish_dialplan_playback(
        &mut self,
        key: &CallKey,
        media: MediaTaskToken,
        completion: PlaybackCompletion,
    ) -> Result<Outcome, ControlError> {
        let Some(entry) = self
            .channels
            .get_mut(&key.id)
            .filter(|entry| entry.call.generation() == key.generation)
        else {
            return Ok(Outcome::empty());
        };
        let Some(frame) = &mut entry.dialplan else {
            return Ok(Outcome::empty());
        };
        let Some(PlaybackWait::Playing {
            token,
            media: expected,
        }) = &frame.playback
        else {
            return Ok(Outcome::empty());
        };
        if *expected != media {
            return Ok(Outcome::empty());
        }
        if !entry.media_tasks.finish(media)
            || !frame
                .machine
                .as_mut()
                .is_some_and(|machine| machine.complete(*token, DialplanContinuation::Advance))
        {
            return Err(ControlError::Conflict("Playback completion changed"));
        }
        frame.playback = None;
        let status = match completion {
            PlaybackCompletion::Completed => "SUCCESS",
            PlaybackCompletion::Failed => "FAILED",
        };
        entry
            .variables
            .insert("PLAYBACKSTATUS".into(), status.into());
        Ok(Outcome::empty())
    }

    fn queue_dialplan_playback(
        &mut self,
        key: &CallKey,
        token: DialplanToken,
        name: String,
        outcome: &mut Outcome,
    ) -> Result<PlaybackWait, ControlError> {
        let entry = self
            .channels
            .get_mut(&key.id)
            .ok_or(ControlError::NotFound("Channel not found"))?;
        let media = entry.media_tasks.reserve(MediaTaskKind::Playback)?;
        if let Err(error) = entry.media_tasks.start(media) {
            let _released = entry.media_tasks.finish(media);
            return Err(error.into());
        }
        outcome.effects.push(Effect::Playback {
            key: key.clone(),
            token: media,
            media: name,
        });
        Ok(PlaybackWait::Playing { token, media })
    }
    pub(super) fn continue_dialplan(
        &mut self,
        id: &ChannelId,
        context: Option<String>,
        extension: Option<String>,
        priority: Option<u16>,
        timestamp: &str,
    ) -> Result<Outcome, ControlError> {
        let entry = self
            .channels
            .get(id)
            .ok_or(ControlError::NotFound("Channel not found"))?;
        let StasisMembership::Entered(application) = entry.call.stasis() else {
            return Err(ControlError::Conflict("Channel not in Stasis application"));
        };
        if matches!(
            entry.snapshot.state,
            ChannelState::Down | ChannelState::Rsrved | ChannelState::Ringing
        ) {
            return Err(ControlError::InvalidState("Channel in invalid state"));
        }
        let context = context.filter(|value| !value.is_empty());
        let extension = extension.filter(|value| !value.is_empty());
        if context
            .iter()
            .chain(extension.iter())
            .any(|value| value.len() > 128 || value.chars().any(char::is_control))
        {
            return Err(ControlError::Invalid("Invalid dialplan location"));
        }
        let rank = match priority.filter(|priority| *priority != 0) {
            Some(priority) => priority,
            None if context.is_none() && extension.is_none() => {
                u16::try_from(entry.snapshot.dialplan.priority)
                    .ok()
                    .and_then(|rank| rank.checked_add(1))
                    .ok_or(ControlError::Invalid("Invalid dialplan priority"))?
            }
            None => 1,
        };
        let extension = extension.as_deref().unwrap_or_else(|| {
            if context.is_some() {
                "s"
            } else {
                &entry.snapshot.dialplan.exten
            }
        });
        let context = context
            .as_deref()
            .unwrap_or(&entry.snapshot.dialplan.context);
        let frame = entry
            .dialplan
            .as_ref()
            .ok_or(ControlError::Conflict("Dialplan is executing"))?;
        let destination = frame.program.position(context, extension, rank);
        let extension = extension.to_owned();
        let next_machine = if let Some(mut machine) = frame.machine {
            let DialplanState::Awaiting { position, token } = machine.state() else {
                return Err(ControlError::Conflict("Dialplan is not waiting in Stasis"));
            };
            if !frame
                .program
                .steps
                .get(usize::from(position))
                .is_some_and(|step| matches!(step.application, Application::Stasis { .. }))
            {
                return Err(ControlError::Conflict("Dialplan is not waiting in Stasis"));
            }
            let continuation = destination
                .map(DialplanContinuation::Jump)
                .unwrap_or(DialplanContinuation::End);
            if !machine.complete(token, continuation) {
                return Err(ControlError::Conflict("Dialplan continuation changed"));
            }
            Some(machine)
        } else {
            destination
                .map(|index| {
                    DialplanMachine::new(
                        entry.call.generation(),
                        index,
                        u16::try_from(frame.program.steps.len())
                            .map_err(|_| ControlError::Capacity)?,
                    )
                    .map_err(core_error)
                })
                .transpose()?
        };
        let application_name = super::application_name(&self.applications, entry.call.stasis())?
            .ok_or(ControlError::NotFound("Application not found"))?
            .to_owned();
        let bridge = entry
            .call
            .bridge()
            .map(|token| {
                self.bridges
                    .iter()
                    .find_map(|(id, bridge)| (bridge.token == token).then_some(id.clone()))
                    .ok_or_else(|| {
                        ControlError::Adapter("Channel bridge ownership is missing".into())
                    })
            })
            .transpose()?;
        let mut outcome = match bridge {
            Some(bridge) => {
                self.membership(bridge, [id.clone()].into_iter().collect(), false, timestamp)?
            }
            None => Outcome::empty(),
        };
        let entry = self
            .channels
            .get_mut(id)
            .ok_or(ControlError::NotFound("Channel not found"))?;
        entry.call = entry.call.leave_stasis(application)?;
        entry
            .variables
            .insert("STASISSTATUS".into(), "SUCCESS".into());
        let frame = entry
            .dialplan
            .as_mut()
            .ok_or(ControlError::Conflict("Dialplan is executing"))?;
        frame.machine = next_machine;
        frame.extension = extension;
        frame.answer = None;
        outcome.event(
            &application_name,
            timestamp,
            EventKind::StasisEnd {
                channel: entry.snapshot.clone(),
            },
        );
        if next_machine.is_none() {
            outcome.append(self.end(id, timestamp, 16)?);
        }
        Ok(outcome)
    }

    pub fn start_dialplan(
        &mut self,
        key: &CallKey,
        program: Dialplan,
        now: u64,
        timestamp: &str,
    ) -> Result<Outcome, ControlError> {
        let entry = self
            .channels
            .get_mut(&key.id)
            .ok_or(ControlError::NotFound("Channel not found"))?;
        if entry.call.generation() != key.generation {
            return Err(ControlError::Conflict("Channel generation changed"));
        }
        if entry
            .dialplan
            .as_ref()
            .is_some_and(|frame| frame.machine.is_some())
        {
            return Err(ControlError::Conflict("Dialplan already started"));
        }
        let location = &entry.snapshot.dialplan;
        let index = program
            .position(
                &location.context,
                &location.exten,
                u16::try_from(location.priority)
                    .map_err(|_| ControlError::Invalid("Invalid dialplan priority"))?,
            )
            .ok_or(ControlError::NotFound("Extension not found"))?;
        let length = u16::try_from(program.steps.len()).map_err(|_| ControlError::Capacity)?;
        let machine = DialplanMachine::new(key.generation, index, length).map_err(core_error)?;
        entry.dialplan = Some(DialplanFrame {
            program,
            extension: location.exten.clone(),
            machine: Some(machine),
            answer: None,
            dial: None,
            playback: None,
        });
        self.advance_dialplan(key, now, timestamp)
    }

    pub fn tick_dialplans(&mut self, now: u64, timestamp: &str) -> Result<Outcome, ControlError> {
        let keys = self
            .channels
            .iter_mut()
            .filter_map(|(id, entry)| {
                entry
                    .dialplan
                    .as_mut()
                    .and_then(|frame| frame.machine.as_mut())
                    .map(|machine| {
                        machine.begin_turn();
                        machine.tick(now);
                        CallKey {
                            id: id.clone(),
                            generation: entry.call.generation(),
                        }
                    })
            })
            .collect::<Vec<_>>();
        let mut outcome = Outcome::empty();
        for key in keys {
            outcome.append(self.advance_dialplan(&key, now, timestamp)?);
        }
        Ok(outcome)
    }

    fn expand(&self, key: &CallKey, source: &str) -> Result<String, ControlError> {
        let entry = self
            .channels
            .get(&key.id)
            .ok_or(ControlError::NotFound("Channel not found"))?;
        expand_variables(source, |name| match entry.variable(name) {
            Err(ControlError::NotFound(_)) => Ok(String::new()),
            result => result,
        })
        .map_err(|error| match error {
            ExpansionError::Syntax(reason) => ControlError::Invalid(reason),
            ExpansionError::Capacity => ControlError::Capacity,
            ExpansionError::Resolve(error) => error,
        })
    }

    fn advance_dialplan(
        &mut self,
        key: &CallKey,
        now: u64,
        timestamp: &str,
    ) -> Result<Outcome, ControlError> {
        let frame = self
            .channels
            .get_mut(&key.id)
            .filter(|entry| entry.call.generation() == key.generation)
            .and_then(|entry| entry.dialplan.take());
        let Some(mut frame) = frame else {
            return Ok(Outcome::empty());
        };
        let mut outcome = Outcome::empty();
        let result = self.execute_dialplan(key, &mut frame, now, timestamp, &mut outcome);
        match result {
            Ok(()) => {
                if let Some(entry) = self.channels.get_mut(&key.id) {
                    entry.dialplan = Some(frame);
                }
            }
            Err(error) => {
                tracing::warn!(channel = %key.id, generation = key.generation, %error, "dialplan failed");
                if let Some(machine) = &mut frame.machine {
                    machine.cancel();
                }
                if let Some(entry) = self.channels.get_mut(&key.id) {
                    entry.dialplan = Some(frame);
                    outcome.append(self.end(&key.id, timestamp, 21)?);
                }
            }
        }
        Ok(outcome)
    }

    fn execute_dialplan(
        &mut self,
        key: &CallKey,
        frame: &mut DialplanFrame,
        now: u64,
        timestamp: &str,
        outcome: &mut Outcome,
    ) -> Result<(), ControlError> {
        let Some(machine) = &mut frame.machine else {
            return Ok(());
        };
        loop {
            if let Some(dial) = &mut frame.dial {
                let decision = dial.fork.tick(now);
                if decision.outcome == DialForkOutcome::Continue {
                    outcome.append(self.release_dial(dial, decision.release, timestamp)?);
                    if !machine.complete(dial.token, DialplanContinuation::Advance) {
                        return Err(ControlError::Conflict("Dial completion changed"));
                    }
                    outcome.append(self.transfer_dial_failed(
                        key,
                        dial.token,
                        DialFailure::NoAnswer,
                    ));
                    frame.dial = None;
                    self.channels
                        .get_mut(&key.id)
                        .ok_or(ControlError::NotFound("Channel not found"))?
                        .variables
                        .insert("DIALSTATUS".into(), "NOANSWER".into());
                }
            }
            if let Some(answer) = &mut frame.answer {
                let entry = self
                    .channels
                    .get(&key.id)
                    .ok_or(ControlError::NotFound("Channel not found"))?;
                match answer
                    .observe(now, entry.call.state(), entry.call.media_connection())
                    .map_err(core_error)?
                {
                    AnswerStatus::Pending => return Ok(()),
                    AnswerStatus::Ready => {
                        if let Some(playback) = frame.playback.take() {
                            match playback {
                                PlaybackWait::Answering { token, name } => {
                                    frame.playback = Some(
                                        self.queue_dialplan_playback(key, token, name, outcome)?,
                                    );
                                    frame.answer = None;
                                    return Ok(());
                                }
                                PlaybackWait::Playing { .. } => {
                                    return Err(ControlError::Conflict("Playback answer changed"))
                                }
                            }
                        }
                        let DialplanState::Awaiting { token, .. } = machine.state() else {
                            return Err(ControlError::Conflict(
                                "Answer lost its dialplan operation",
                            ));
                        };
                        if !machine.complete(token, DialplanContinuation::Advance) {
                            return Err(ControlError::Conflict("Answer completion changed"));
                        }
                        frame.answer = None;
                    }
                    AnswerStatus::Cancelled => machine.cancel(),
                }
            }
            let index = match machine.state() {
                DialplanState::Ready { position } => position,
                DialplanState::Awaiting { .. } | DialplanState::Waiting { .. } => return Ok(()),
                DialplanState::Ended => {
                    outcome.append(self.end(&key.id, timestamp, 16)?);
                    return Ok(());
                }
            };
            let step = frame
                .program
                .steps
                .get(usize::from(index))
                .ok_or(ControlError::Invalid("Invalid dialplan position"))?
                .clone();
            let instruction = match &step.application {
                Application::NoOp(_) => DialplanInstruction::NoOp,
                Application::Set { .. } => DialplanInstruction::Set,
                Application::Answer => DialplanInstruction::Answer,
                Application::Dial { .. } => DialplanInstruction::Dial,
                Application::Stasis { .. } => DialplanInstruction::Stasis,
                Application::Playback(_) => DialplanInstruction::Playback,
                Application::Wait(duration) => DialplanInstruction::Wait(duration.milliseconds()),
                Application::Goto { .. } => DialplanInstruction::Goto,
                Application::Hangup => DialplanInstruction::Hangup,
            };
            let entry = self
                .channels
                .get_mut(&key.id)
                .ok_or(ControlError::NotFound("Channel not found"))?;
            entry.snapshot.dialplan.context.clone_from(&step.context);
            entry.snapshot.dialplan.exten.clone_from(&frame.extension);
            entry.snapshot.dialplan.priority = i32::from(step.priority);
            let mut jump_extension = None;
            let next = match &step.application {
                Application::Goto {
                    context,
                    extension,
                    priority,
                } => {
                    let context = self.expand(key, context.as_deref().unwrap_or(&step.context))?;
                    let extension =
                        self.expand(key, extension.as_deref().unwrap_or(&frame.extension))?;
                    let next = frame.program.position(&context, &extension, *priority);
                    jump_extension = Some(extension);
                    next
                }
                Application::NoOp(_)
                | Application::Set { .. }
                | Application::Answer
                | Application::Dial { .. }
                | Application::Stasis { .. }
                | Application::Playback(_)
                | Application::Wait(_)
                | Application::Hangup => step.priority.checked_add(1).and_then(|priority| {
                    frame
                        .program
                        .position(&step.context, &frame.extension, priority)
                }),
            };
            let execution = match machine
                .dispatch(instruction, next, now)
                .map_err(core_error)?
            {
                DialplanDispatch::Execute(execution) => execution,
                DialplanDispatch::Suspended | DialplanDispatch::Yield => return Ok(()),
                DialplanDispatch::Ended => {
                    outcome.append(self.end(&key.id, timestamp, 16)?);
                    return Ok(());
                }
            };
            if let Some(extension) = jump_extension {
                frame.extension = extension;
            }
            match step.application {
                Application::NoOp(value) => {
                    let data = self.expand(key, &value)?;
                    self.set_dialplan_location(key, "NoOp", data, timestamp, outcome)?;
                }
                Application::Set { variable, value } => {
                    let value = self.expand(key, &value)?;
                    self.set_dialplan_location(
                        key,
                        "Set",
                        format!("{variable}={value}"),
                        timestamp,
                        outcome,
                    )?;
                    outcome.append(self.set_variable(
                        &key.id,
                        variable,
                        Some(value),
                        timestamp,
                        VariableAccess::Dialplan,
                    )?);
                }
                Application::Answer => {
                    self.set_dialplan_location(key, "Answer", String::new(), timestamp, outcome)?;
                    let entry = self
                        .channels
                        .get(&key.id)
                        .ok_or(ControlError::NotFound("Channel not found"))?;
                    if entry.call.state() != CallState::Up {
                        frame.answer = Some(AnswerProgress::new());
                        outcome.effects.push(Effect::Answer { key: key.clone() });
                        return Ok(());
                    }
                }
                Application::Goto {
                    context,
                    extension,
                    priority,
                } => {
                    let arguments = context
                        .into_iter()
                        .chain(extension)
                        .chain(std::iter::once(priority.to_string()))
                        .collect::<Vec<_>>()
                        .join(",");
                    let data = self.expand(key, &arguments)?;
                    self.set_dialplan_location(key, "Goto", data, timestamp, outcome)?;
                }
                Application::Wait(duration) => {
                    self.set_dialplan_location(
                        key,
                        "Wait",
                        duration.argument().to_owned(),
                        timestamp,
                        outcome,
                    )?;
                    machine.tick(now);
                    continue;
                }
                Application::Stasis {
                    application,
                    arguments,
                } => {
                    let raw = std::iter::once(application.as_str())
                        .chain(arguments.iter().map(String::as_str))
                        .collect::<Vec<_>>()
                        .join(",");
                    let expanded = self.expand(key, &raw)?;
                    let mut parts = expanded.split(',');
                    let application = parts
                        .next()
                        .ok_or(ControlError::Invalid("Invalid Stasis arguments"))?;
                    let arguments = parts.map(String::from).collect::<Vec<_>>();
                    if arguments.len() > 32 || arguments.iter().any(|value| value.len() > 256) {
                        return Err(ControlError::Capacity);
                    }
                    self.set_dialplan_location(
                        key,
                        "Stasis",
                        expanded.clone(),
                        timestamp,
                        outcome,
                    )?;
                    self.route_resource_events(outcome);
                    let entry = self
                        .channels
                        .get_mut(&key.id)
                        .ok_or(ControlError::NotFound("Channel not found"))?;
                    if let Some(token) = self.applications.get(application).copied() {
                        entry.variables.insert("STASISSTATUS".into(), String::new());
                        entry.call = entry.call.enter_stasis(token)?;
                        entry.arguments = arguments.clone();
                        outcome.event(
                            application,
                            timestamp,
                            EventKind::ChannelCreated {
                                channel: entry.snapshot.clone(),
                            },
                        );
                        outcome.event(
                            application,
                            timestamp,
                            EventKind::StasisStart {
                                channel: entry.snapshot.clone(),
                                args: arguments,
                            },
                        );
                        return Ok(());
                    }
                    entry
                        .variables
                        .insert("STASISSTATUS".into(), "FAILED".into());
                }
                Application::Hangup => {
                    self.set_dialplan_location(key, "Hangup", String::new(), timestamp, outcome)?;
                    outcome.append(self.end(&key.id, timestamp, 16)?);
                    return Ok(());
                }
                Application::Dial {
                    destinations,
                    timeout,
                } => {
                    let destinations = self.expand(key, &destinations.join("&"))?;
                    let data = match timeout {
                        Some(timeout) => format!("{destinations},{timeout}"),
                        None => destinations.clone(),
                    };
                    self.set_dialplan_location(key, "Dial", data, timestamp, outcome)?;
                    match self.begin_dial(
                        key,
                        execution.token,
                        &destinations,
                        timeout,
                        now,
                        timestamp,
                    ) {
                        Ok((dial, started)) => {
                            frame.dial = Some(dial);
                            outcome.append(started);
                            return Ok(());
                        }
                        Err(
                            ControlError::Capacity
                            | ControlError::NotFound(_)
                            | ControlError::Unsupported,
                        ) => {
                            self.channels
                                .get_mut(&key.id)
                                .ok_or(ControlError::NotFound("Channel not found"))?
                                .variables
                                .insert("DIALSTATUS".into(), "CHANUNAVAIL".into());
                        }
                        Err(error) => return Err(error),
                    }
                }
                Application::Playback(name) => {
                    let name = self.expand(key, &name)?;
                    self.set_dialplan_location(key, "Playback", name.clone(), timestamp, outcome)?;
                    let entry = self
                        .channels
                        .get(&key.id)
                        .ok_or(ControlError::NotFound("Channel not found"))?;
                    if entry.call.state() == CallState::Up {
                        frame.playback = Some(self.queue_dialplan_playback(
                            key,
                            execution.token,
                            name,
                            outcome,
                        )?);
                    } else {
                        frame.playback = Some(PlaybackWait::Answering {
                            token: execution.token,
                            name,
                        });
                        frame.answer = Some(AnswerProgress::new());
                        outcome.effects.push(Effect::Answer { key: key.clone() });
                    }
                    return Ok(());
                }
            }
            if !machine.complete(execution.token, DialplanContinuation::Advance) {
                return Err(ControlError::Conflict("Dialplan completion changed"));
            }
        }
    }

    fn set_dialplan_location(
        &mut self,
        key: &CallKey,
        name: &str,
        data: String,
        timestamp: &str,
        outcome: &mut Outcome,
    ) -> Result<(), ControlError> {
        let entry = self
            .channels
            .get_mut(&key.id)
            .ok_or(ControlError::NotFound("Channel not found"))?;
        entry.snapshot.dialplan.app_name = name.into();
        entry.snapshot.dialplan.app_data = data;
        if !self.resource_subscriptions.is_empty() {
            outcome.notice(
                timestamp,
                &EventKind::ChannelDialplan {
                    channel: entry.snapshot.clone(),
                    dialplan_app: entry.snapshot.dialplan.app_name.clone(),
                    dialplan_app_data: entry.snapshot.dialplan.app_data.clone(),
                },
            );
        }
        Ok(())
    }
}
