use super::{CallKey, ChannelEntry, ControlError};
use rusterisk_api::ChannelId;
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct CallHandle(u64);

impl CallHandle {
    pub(crate) const fn new(generation: u64) -> Self {
        Self(generation)
    }

    pub(crate) const fn slot(self) -> u8 {
        (self.0.wrapping_sub(1) % 64) as u8
    }
}

impl CallKey {
    pub(crate) const fn handle(&self) -> CallHandle {
        CallHandle::new(self.generation)
    }
}

#[derive(Default)]
pub(super) struct ChannelSlots {
    names: BTreeMap<ChannelId, CallHandle>,
    rows: BTreeMap<u8, (ChannelId, ChannelEntry)>,
}

impl ChannelSlots {
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn available(&self, handle: CallHandle) -> bool {
        !self.rows.contains_key(&handle.slot())
    }

    pub fn contains_key(&self, id: &ChannelId) -> bool {
        self.names.contains_key(id)
    }

    pub fn get(&self, id: &ChannelId) -> Option<&ChannelEntry> {
        self.by_handle(*self.names.get(id)?)
    }

    pub fn get_mut(&mut self, id: &ChannelId) -> Option<&mut ChannelEntry> {
        self.by_handle_mut(*self.names.get(id)?)
    }

    pub fn by_handle(&self, handle: CallHandle) -> Option<&ChannelEntry> {
        let (_, entry) = self.rows.get(&handle.slot())?;
        (entry.call.generation() == handle.0).then_some(entry)
    }

    pub fn by_handle_mut(&mut self, handle: CallHandle) -> Option<&mut ChannelEntry> {
        let (_, entry) = self.rows.get_mut(&handle.slot())?;
        (entry.call.generation() == handle.0).then_some(entry)
    }

    pub fn require_key_mut(&mut self, key: &CallKey) -> Result<&mut ChannelEntry, ControlError> {
        let current = self
            .names
            .get(&key.id)
            .ok_or(ControlError::NotFound("Channel not found"))?;
        if *current != key.handle() {
            return Err(ControlError::Conflict("Channel generation changed"));
        }
        self.by_handle_mut(key.handle())
            .ok_or(ControlError::NotFound("Channel not found"))
    }

    pub fn by_key(&self, key: &CallKey) -> Option<&ChannelEntry> {
        self.by_handle(key.handle())
            .filter(|entry| entry.snapshot.id == key.id)
    }

    pub fn by_key_mut(&mut self, key: &CallKey) -> Option<&mut ChannelEntry> {
        self.by_handle_mut(key.handle())
            .filter(|entry| entry.snapshot.id == key.id)
    }

    pub fn insert(&mut self, id: ChannelId, entry: ChannelEntry) -> Result<(), ControlError> {
        let handle = CallHandle::new(entry.call.generation());
        if !self.available(handle) || self.names.contains_key(&id) {
            return Err(ControlError::Capacity);
        }
        self.names.insert(id.clone(), handle);
        self.rows.insert(handle.slot(), (id, entry));
        Ok(())
    }

    pub fn remove(&mut self, id: &ChannelId) -> Option<ChannelEntry> {
        let handle = self.names.remove(id)?;
        self.rows.remove(&handle.slot()).map(|(_, entry)| entry)
    }

    pub fn keys(&self) -> impl Iterator<Item = &ChannelId> {
        self.names.keys()
    }

    pub fn values(&self) -> impl Iterator<Item = &ChannelEntry> {
        self.iter().map(|(_, entry)| entry)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&ChannelId, &ChannelEntry)> {
        self.names
            .iter()
            .filter_map(|(id, handle)| Some((id, self.by_handle(*handle)?)))
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = (&ChannelId, &mut ChannelEntry)> {
        self.rows.values_mut().map(|(id, entry)| (&*id, entry))
    }
}

#[cfg(test)]
impl std::ops::Index<&ChannelId> for ChannelSlots {
    type Output = ChannelEntry;

    fn index(&self, id: &ChannelId) -> &Self::Output {
        &self.rows[&self.names[id].slot()].1
    }
}
