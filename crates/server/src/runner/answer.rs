use super::{timestamp, Controller, Credentials, Request, Runner};
use crate::runner::calls;
use crate::{Action, CallKey, Control, ControlError, Outcome};
use rusterisk_api::{ChannelId, ChannelState};
use rusterisk_core::Wake;
use std::collections::VecDeque;
use tokio::sync::oneshot;

type Reply = oneshot::Sender<Result<Outcome, ControlError>>;

#[cfg(test)]
mod tests;

pub(super) struct Pending {
    deadline: u64,
    replies: VecDeque<(u64, Reply)>,
}

fn answered(control: &Control, key: &CallKey) -> Result<bool, ControlError> {
    if !control.has_channel(key) {
        return Err(ControlError::NotFound("Channel ended while answering"));
    }
    control
        .channel(&key.id)
        .map(|channel| channel.state == ChannelState::Up)
}

impl Pending {
    pub(super) fn wake(&self) -> Wake {
        self.replies
            .iter()
            .fold(Wake::At(self.deadline), |wake, (deadline, _)| {
                wake.earliest(Wake::At(*deadline))
            })
    }
}

impl Controller {
    pub(crate) async fn answer_channel(
        &self,
        credentials: Credentials,
        origin: Option<String>,
        channel: ChannelId,
    ) -> Result<Outcome, ControlError> {
        let (reply, receiver) = oneshot::channel();
        self.requests
            .try_send(Request::Answer {
                credentials,
                origin,
                channel,
                reply,
            })
            .map_err(ControlError::queue)?;
        self.receive(receiver).await
    }
}

fn respond(reply: Reply, result: Result<(), ControlError>) {
    if reply.send(result.map(|()| Outcome::empty())).is_err() {
        tracing::debug!("Answer requester disconnected");
    }
}

impl Runner {
    pub(super) fn request_answer(
        &mut self,
        credentials: Credentials,
        origin: Option<String>,
        channel: ChannelId,
        reply: Reply,
        now: u64,
    ) {
        if reply.is_closed() {
            return;
        }
        let result = self
            .authorize(&credentials, origin.as_deref(), false)
            .and_then(|()| self.start_answer(channel, now));
        match result {
            Ok(Some(pending)) => pending.replies.push_back((pending.deadline, reply)),
            Ok(None) => respond(reply, Ok(())),
            Err(error) => respond(reply, Err(error)),
        }
    }

    fn start_answer(
        &mut self,
        channel: ChannelId,
        now: u64,
    ) -> Result<Option<&mut Pending>, ControlError> {
        let key = self.control.key(&channel)?;
        let mut outcome = self
            .control
            .apply(Action::Answer(channel), &timestamp(), now)?;
        if outcome.effects.is_empty() {
            return Ok(None);
        }
        let deadline = now.checked_add(5000).ok_or(ControlError::Capacity)?;
        if self
            .records
            .iter(calls::ANSWER)
            .map(|(_, value)| value)
            .map(|pending| pending.replies.len())
            .sum::<usize>()
            >= 64
        {
            return Err(ControlError::Capacity);
        }
        if !self.records.get(calls::ANSWER, &key).is_some() {
            if self.records.iter(calls::ANSWER).count() >= 64
                || !self.effects_flowing()
                || self.effect_room() < outcome.effects.len()
            {
                return Err(ControlError::Capacity);
            }
            self.publish(&mut outcome)?;
        }
        let pending = self.records.get_or_insert(calls::ANSWER, key, || Pending {
            deadline,
            replies: VecDeque::new(),
        })?;
        pending.deadline = deadline;
        Ok(Some(pending))
    }

    pub(super) fn answer_waits(&self, key: &CallKey) -> bool {
        answered(&self.control, key) == Ok(false)
    }

    pub(super) fn finish_answers(&mut self, now: u64) {
        self.records.retain(calls::ANSWER, |key, pending| {
            let state = answered(&self.control, key);
            for _ in 0..pending.replies.len() {
                let Some((deadline, reply)) = pending.replies.pop_front() else {
                    break;
                };
                if reply.is_closed() {
                    continue;
                }
                match &state {
                    Err(error) => respond(reply, Err(error.clone())),
                    Ok(_) if now >= deadline => respond(reply, Err(ControlError::Capacity)),
                    Ok(true) => respond(reply, Ok(())),
                    Ok(false) => pending.replies.push_back((deadline, reply)),
                }
            }
            state == Ok(false) && now < pending.deadline
        });
    }
}
