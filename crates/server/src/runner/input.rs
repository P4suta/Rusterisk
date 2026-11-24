use super::{Request, Runner};
use crate::storage::WorkerError;
use crate::{ControlError, Signal};
use tokio::task::JoinError;

// One input of the control loop, which another loop can wait for beside its own inputs.
pub(crate) struct Input(Kind);

enum Kind {
    // A reload input needs no time, so waiting applies it at once.
    Reloaded(Result<(), ControlError>),
    Stored(Option<Result<(), JoinError>>),
    History(Option<Result<Result<(), WorkerError>, JoinError>>),
    Stopped,
    Signal(Option<Signal>),
    Request(Option<Request>),
    Closure(Option<()>),
    Unchanged,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Flow {
    Continue,
    Stop,
}

impl Runner {
    // Waits for the next input, and reads a signal only when the caller accepts one.
    // Every branch is cancel-safe, and nothing awaits after the first input arrives.
    pub(crate) async fn next(&mut self, signals: bool) -> Input {
        let Self {
            reload,
            stored_jobs,
            history_jobs,
            shutdown,
            signals: signal_queue,
            requests,
            closed,
            closures_open,
            ..
        } = self;
        let kind = tokio::select! {
            loaded = reload.loaders.join_next(), if !reload.loaders.is_empty() => match loaded {
                Some(result) => return Input(Kind::Reloaded(self.loading_completed(result))),
                None => Kind::Reloaded(Err(ControlError::Conflict("Settings loading task missing"))),
            },
            prepared = reload.preparations.join_next(), if !reload.preparations.is_empty() => match prepared {
                Some(result) => return Input(Kind::Reloaded(self.preparation_completed(result))),
                None => Kind::Reloaded(Err(ControlError::Conflict("Settings preparation task missing"))),
            },
            step = reload.steps.join_next(), if !reload.steps.is_empty() => match step {
                Some(result) => return Input(Kind::Reloaded(self.publication_completed(result))),
                None => Kind::Reloaded(Err(ControlError::Conflict("Settings acknowledgement task missing"))),
            },
            replacement = reload.requests.recv(), if !reload.requests.is_closed() || !reload.requests.is_empty() => {
                if let Some(replacement) = replacement {
                    self.begin_reload(replacement);
                }
                Kind::Reloaded(Ok(()))
            }
            request = reload.directory_requests.recv(), if !reload.directory_requests.is_closed() || !reload.directory_requests.is_empty() => {
                if let Some(request) = request {
                    self.begin_directory_reload(request);
                }
                Kind::Reloaded(Ok(()))
            }
            result = stored_jobs.join_next(), if !stored_jobs.is_empty() => Kind::Stored(result),
            result = history_jobs.join_next(), if !history_jobs.is_empty() => Kind::History(result),
            changed = shutdown.changed() => if changed.is_err() || *shutdown.borrow() { Kind::Stopped } else { Kind::Unchanged },
            signal = signal_queue.recv(), if signals => Kind::Signal(signal),
            request = requests.recv() => Kind::Request(request),
            closure = closed.recv(), if *closures_open => Kind::Closure(closure),
        };
        Input(kind)
    }

    // Applies one input, and reports whether the control loop continues.
    pub(crate) fn handle(&mut self, input: Input, now: u64) -> Result<Flow, ControlError> {
        match input.0 {
            Kind::Reloaded(result) => result?,
            Kind::Stored(result) => {
                result
                    .ok_or(ControlError::Capacity)?
                    .map_err(|error| ControlError::Adapter(error.to_string()))?;
            }
            Kind::History(result) => {
                result
                    .ok_or(ControlError::Capacity)?
                    .map_err(|error| ControlError::Adapter(error.to_string()))?
                    .map_err(|error| ControlError::Adapter(error.to_string()))?;
            }
            Kind::Stopped | Kind::Signal(None) | Kind::Request(None) => return Ok(Flow::Stop),
            Kind::Signal(Some(signal)) => self.apply_signal(signal, now)?,
            Kind::Request(Some(request)) => self.request(request, now)?,
            Kind::Closure(closure) => self.closures_open = closure.is_some(),
            Kind::Unchanged => {}
        }
        Ok(Flow::Continue)
    }
}
