use super::{application_name, Control, ControlError, Outcome};
use rusterisk_api::{Application, ChannelId, EventKind};
use rusterisk_core::StasisMembership;

impl Control {
    pub(super) fn application(&self, name: &str) -> Result<Application, ControlError> {
        let application = self
            .applications
            .get(name)
            .copied()
            .ok_or(ControlError::NotFound("Application not found"))?;
        let channel_ids: Vec<ChannelId> = self.channels.iter().filter(|(_, entry)| matches!(entry.call.stasis(), StasisMembership::Pending(current) | StasisMembership::Entered(current) if current == application)).map(|(id, _)| id.clone()).collect();
        let bridge_ids = self
            .bridges
            .iter()
            .filter(|(_, bridge)| {
                bridge
                    .snapshot
                    .channels
                    .iter()
                    .any(|id| channel_ids.contains(id))
            })
            .map(|(id, _)| id.clone())
            .collect();
        Ok(Application {
            name: name.into(),
            channel_ids,
            bridge_ids,
            endpoint_ids: Vec::new(),
            device_names: Vec::new(),
            events_allowed: Vec::new(),
            events_disallowed: Vec::new(),
        })
    }

    pub(super) fn move_application(
        &mut self,
        id: &ChannelId,
        app: String,
        args: Vec<String>,
        timestamp: &str,
    ) -> Result<Outcome, ControlError> {
        let entry = self
            .channels
            .get_mut(id)
            .ok_or(ControlError::NotFound("Channel not found"))?;
        let StasisMembership::Entered(previous) = entry.call.stasis() else {
            return Err(ControlError::Conflict("Channel not in Stasis application"));
        };
        ChannelId::try_from(app.clone())?;
        if args.len() > 32 || args.iter().any(|value| value.len() > 256) {
            return Err(ControlError::Invalid("Invalid Stasis arguments"));
        }
        let previous_name = application_name(&self.applications, entry.call.stasis())?
            .ok_or(ControlError::NotFound("Application not found"))?;
        let mut outcome = Outcome::empty();
        let Some(destination) = self.applications.get(&app).copied() else {
            outcome.event(
                previous_name,
                timestamp,
                EventKind::ApplicationMoveFailed {
                    channel: entry.snapshot.clone(),
                    destination: app,
                    args,
                },
            );
            return Ok(outcome);
        };
        entry.call = entry.call.move_stasis(previous, destination)?;
        entry.arguments = args.clone();
        outcome.event(
            previous_name,
            timestamp,
            EventKind::StasisEnd {
                channel: entry.snapshot.clone(),
            },
        );
        outcome.event(
            &app,
            timestamp,
            EventKind::StasisStart {
                channel: entry.snapshot.clone(),
                args,
            },
        );
        Ok(outcome)
    }
}
