use super::{CallKey, Control, ControlError, Effect, Outcome, VariableAccess};
use rusterisk_api::EventKind;
use rusterisk_config::{expand_variables, Application, Dialplan, ExpansionError};
use rusterisk_core::{
    AnswerProgress, AnswerStatus, CallState, DialplanContinuation, DialplanDispatch, DialplanError,
    DialplanInstruction, DialplanMachine, DialplanState,
};

pub(super) struct DialplanFrame {
    program: Dialplan,
    machine: DialplanMachine,
    answer: Option<AnswerProgress>,
}

fn core_error(error: DialplanError) -> ControlError {
    match error {
        DialplanError::Program => ControlError::Invalid("Invalid dialplan position"),
        DialplanError::Clock => ControlError::Adapter("Dialplan timer overflow".into()),
        DialplanError::Capacity => ControlError::Capacity,
    }
}

fn position(program: &Dialplan, context: &str, extension: &str, priority: u16) -> Option<u16> {
    program
        .steps
        .iter()
        .position(|step| {
            step.context == context && step.extension == extension && step.priority == priority
        })
        .and_then(|index| u16::try_from(index).ok())
}

impl Control {
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
        if entry.dialplan.is_some() {
            return Err(ControlError::Conflict("Dialplan already started"));
        }
        let location = &entry.snapshot.dialplan;
        let index = position(
            &program,
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
            machine,
            answer: None,
        });
        self.advance_dialplan(key, now, timestamp)
    }

    pub fn tick_dialplans(&mut self, now: u64, timestamp: &str) -> Result<Outcome, ControlError> {
        let keys = self
            .channels
            .iter_mut()
            .filter_map(|(id, entry)| {
                entry.dialplan.as_mut().map(|frame| {
                    frame.machine.begin_turn();
                    frame.machine.tick(now);
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
                frame.machine.cancel();
                if self.channels.contains_key(&key.id) {
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
        loop {
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
                        let DialplanState::Awaiting { token, .. } = frame.machine.state() else {
                            return Err(ControlError::Conflict(
                                "Answer lost its dialplan operation",
                            ));
                        };
                        if !frame.machine.complete(token, DialplanContinuation::Advance) {
                            return Err(ControlError::Conflict("Answer completion changed"));
                        }
                        frame.answer = None;
                    }
                    AnswerStatus::Cancelled => frame.machine.cancel(),
                }
            }
            let index = match frame.machine.state() {
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
                Application::Wait(seconds) => DialplanInstruction::Wait(u32::from(*seconds) * 1000),
                Application::Goto { .. } => DialplanInstruction::Goto,
                Application::Hangup => DialplanInstruction::Hangup,
            };
            let next = match &step.application {
                Application::Goto {
                    context,
                    extension,
                    priority,
                } => {
                    let context = self.expand(key, context.as_deref().unwrap_or(&step.context))?;
                    let extension =
                        self.expand(key, extension.as_deref().unwrap_or(&step.extension))?;
                    position(&frame.program, &context, &extension, *priority)
                }
                Application::NoOp(_)
                | Application::Set { .. }
                | Application::Answer
                | Application::Dial { .. }
                | Application::Stasis { .. }
                | Application::Playback(_)
                | Application::Wait(_)
                | Application::Hangup => step.priority.checked_add(1).and_then(|priority| {
                    position(&frame.program, &step.context, &step.extension, priority)
                }),
            };
            let execution = match frame
                .machine
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
            let entry = self
                .channels
                .get_mut(&key.id)
                .ok_or(ControlError::NotFound("Channel not found"))?;
            entry.snapshot.dialplan.context = step.context;
            entry.snapshot.dialplan.exten = step.extension;
            entry.snapshot.dialplan.priority = i32::from(step.priority);
            match step.application {
                Application::NoOp(value) => {
                    let data = self.expand(key, &value)?;
                    self.set_dialplan_location(key, "NoOp", data)?;
                }
                Application::Set { variable, value } => {
                    let value = self.expand(key, &value)?;
                    self.set_dialplan_location(key, "Set", format!("{variable}={value}"))?;
                    outcome.append(self.set_variable(
                        &key.id,
                        variable,
                        Some(value),
                        timestamp,
                        VariableAccess::Dialplan,
                    )?);
                }
                Application::Answer => {
                    self.set_dialplan_location(key, "Answer", String::new())?;
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
                    self.set_dialplan_location(
                        key,
                        "Goto",
                        format!(
                            "{},{},{priority}",
                            context.unwrap_or_default(),
                            extension.unwrap_or_default()
                        ),
                    )?;
                }
                Application::Wait(seconds) => {
                    self.set_dialplan_location(key, "Wait", seconds.to_string())?;
                    frame.machine.tick(now);
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
                    let token = self
                        .applications
                        .get(application)
                        .copied()
                        .ok_or(ControlError::NotFound("Application not found"))?;
                    self.set_dialplan_location(key, "Stasis", expanded.clone())?;
                    let entry = self
                        .channels
                        .get_mut(&key.id)
                        .ok_or(ControlError::NotFound("Channel not found"))?;
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
                Application::Hangup => {
                    outcome.append(self.end(&key.id, timestamp, 16)?);
                    return Ok(());
                }
                Application::Dial { .. } | Application::Playback(_) => {
                    return Err(ControlError::Unsupported)
                }
            }
            if !frame
                .machine
                .complete(execution.token, DialplanContinuation::Advance)
            {
                return Err(ControlError::Conflict("Dialplan completion changed"));
            }
        }
    }

    fn set_dialplan_location(
        &mut self,
        key: &CallKey,
        name: &str,
        data: String,
    ) -> Result<(), ControlError> {
        let entry = self
            .channels
            .get_mut(&key.id)
            .ok_or(ControlError::NotFound("Channel not found"))?;
        entry.snapshot.dialplan.app_name = name.into();
        entry.snapshot.dialplan.app_data = data;
        Ok(())
    }
}
