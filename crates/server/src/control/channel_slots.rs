use super::{CallKey, ChannelEntry, ControlError};
use rusterisk_api::ChannelId;
use std::collections::BTreeMap;

use rusterisk_core::{CallHandle, CallTable, Teardown};

impl CallKey {
    pub(crate) const fn handle(&self) -> CallHandle {
        CallHandle::new(self.generation)
    }
}

#[derive(Default)]
pub(super) struct ChannelSlots {
    table: CallTable<64>,
    names: BTreeMap<ChannelId, CallHandle>,
    retiring: BTreeMap<u8, ChannelId>,
    rows: BTreeMap<u8, (ChannelId, ChannelEntry)>,
}

impl ChannelSlots {
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn occupied(&self) -> usize {
        self.rows.len() + self.retiring.len()
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn available(&self, handle: CallHandle) -> bool {
        self.table.available(handle)
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
        (entry.call.generation() == handle.generation()).then_some(entry)
    }

    pub fn by_handle_mut(&mut self, handle: CallHandle) -> Option<&mut ChannelEntry> {
        let (_, entry) = self.rows.get_mut(&handle.slot())?;
        (entry.call.generation() == handle.generation()).then_some(entry)
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
        if self.names.contains_key(&id) || !self.table.admit(handle) {
            return Err(ControlError::Capacity);
        }
        self.names.insert(id.clone(), handle);
        self.rows.insert(handle.slot(), (id, entry));
        Ok(())
    }

    pub fn end(&mut self, id: &ChannelId) -> Option<(ChannelEntry, Teardown)> {
        let handle = *self.names.get(id)?;
        let cleanup = self.table.teardown(handle)?;
        self.names.remove(id);
        let (name, entry) = self.rows.remove(&handle.slot())?;
        self.retiring.insert(handle.slot(), name);
        Some((entry, cleanup))
    }

    pub fn await_media(&mut self, cleanup: Teardown) -> bool {
        self.table.await_media(cleanup)
    }

    pub fn acknowledge(&mut self, key: &CallKey) -> bool {
        let handle = key.handle();
        if self.retiring.get(&handle.slot()) == Some(&key.id) && self.table.acknowledge(handle) {
            self.retiring.remove(&handle.slot());
            return true;
        }
        false
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
