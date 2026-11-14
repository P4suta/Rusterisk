use super::{application_name, Control, ControlError, Outcome};
use rusterisk_api::{ChannelId, EventKind};
use rusterisk_core::StasisMembership;

impl Control {
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
