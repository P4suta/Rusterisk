use super::{application_name, Control, ControlError, Outcome};
use rusterisk_api::{Application, ChannelId, Event, EventFilter, EventFilterRequest, EventKind};
use rusterisk_core::StasisMembership;

#[derive(Default)]
pub(super) struct EventFilters {
    allowed: Vec<EventFilter>,
    disallowed: Vec<EventFilter>,
}

impl Control {
    pub(super) fn filter_application(
        &mut self,
        name: &str,
        update: Option<EventFilterRequest>,
    ) -> Result<Outcome, ControlError> {
        let token = self
            .applications
            .get(name)
            .copied()
            .ok_or(ControlError::NotFound("Application not found"))?;
        if let Some(update) = &update {
            for filters in update.allowed.iter().chain(update.disallowed.iter()) {
                if filters.len() > 128
                    || filters
                        .iter()
                        .any(|filter| filter.event_type.is_empty() || filter.event_type.len() > 128)
                {
                    return Err(ControlError::Invalid("Invalid format definition"));
                }
            }
        }
        match update {
            None => {
                self.event_filters.remove(&token);
            }
            Some(update) => {
                let filters = self.event_filters.entry(token).or_default();
                if let Some(allowed) = update.allowed {
                    filters.allowed = allowed;
                }
                if let Some(disallowed) = update.disallowed {
                    filters.disallowed = disallowed;
                }
            }
        }
        Outcome::json(self.application(name)?)
    }

    pub(crate) fn event_permitted(&self, event: &Event) -> bool {
        let Some(application) = self.applications.get(&event.application) else {
            return false;
        };
        let Some(filters) = self.event_filters.get(application) else {
            return true;
        };
        let kind = event.kind.event_type();
        !filters
            .disallowed
            .iter()
            .any(|filter| filter.event_type == kind)
            && (filters.allowed.is_empty()
                || filters
                    .allowed
                    .iter()
                    .any(|filter| filter.event_type == kind))
    }

    pub(super) fn application(&self, name: &str) -> Result<Application, ControlError> {
        let application = self
            .applications
            .get(name)
            .copied()
            .ok_or(ControlError::NotFound("Application not found"))?;
        let mut channel_ids: Vec<ChannelId> = self.channels.iter().filter(|(_, entry)| matches!(entry.call.stasis(), StasisMembership::Pending(current) | StasisMembership::Entered(current) if current == application)).map(|(id, _)| id.clone()).collect();
        let mut bridge_ids: Vec<ChannelId> = self
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
        self.append_subscribed_resources(application, &mut channel_ids, &mut bridge_ids)?;
        channel_ids.sort();
        channel_ids.dedup();
        bridge_ids.sort();
        bridge_ids.dedup();
        let filters = self.event_filters.get(&application);
        Ok(Application {
            name: name.into(),
            channel_ids,
            bridge_ids,
            endpoint_ids: Vec::new(),
            device_names: Vec::new(),
            events_allowed: filters
                .map(|filters| filters.allowed.clone())
                .unwrap_or_default(),
            events_disallowed: filters
                .map(|filters| filters.disallowed.clone())
                .unwrap_or_default(),
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
