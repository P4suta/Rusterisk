use crate::{CallKey, ControlError};
use std::collections::btree_map::Entry;
use std::collections::BTreeMap;

pub(crate) trait CallRecord: Default {
    fn empty(&self) -> bool;
}

pub(crate) struct Field<R, V> {
    read: fn(&R) -> &Option<V>,
    write: fn(&mut R) -> &mut Option<V>,
}

impl<R, V> Field<R, V> {
    pub(crate) const fn new(
        read: fn(&R) -> &Option<V>,
        write: fn(&mut R) -> &mut Option<V>,
    ) -> Self {
        Self { read, write }
    }
}

pub(crate) struct CallRecords<R>(BTreeMap<u8, (CallKey, R)>);

impl<R: CallRecord> CallRecords<R> {
    pub(crate) fn new() -> Self {
        Self(BTreeMap::new())
    }

    pub(crate) fn get<V>(&self, field: Field<R, V>, key: &CallKey) -> Option<&V> {
        let (current, row) = self.0.get(&key.handle().slot())?;
        if current != key {
            return None;
        }
        (field.read)(row).as_ref()
    }

    pub(crate) fn get_mut<V>(&mut self, field: Field<R, V>, key: &CallKey) -> Option<&mut V> {
        let (current, row) = self.0.get_mut(&key.handle().slot())?;
        if current != key {
            return None;
        }
        (field.write)(row).as_mut()
    }

    fn record(&mut self, key: CallKey) -> Result<&mut R, ControlError> {
        let entry = self.0.entry(key.handle().slot());
        let row = match entry {
            Entry::Vacant(slot) => slot.insert((key, R::default())),
            Entry::Occupied(slot) => {
                let record = slot.into_mut();
                if record.0 != key {
                    if !record.1.empty() {
                        return Err(ControlError::Capacity);
                    }
                    *record = (key, R::default());
                }
                record
            }
        };
        Ok(&mut row.1)
    }

    pub(crate) fn insert<V>(
        &mut self,
        field: Field<R, V>,
        key: CallKey,
        value: V,
    ) -> Result<(), ControlError> {
        *(field.write)(self.record(key)?) = Some(value);
        Ok(())
    }

    pub(crate) fn get_or_insert<V>(
        &mut self,
        field: Field<R, V>,
        key: CallKey,
        create: impl FnOnce() -> V,
    ) -> Result<&mut V, ControlError> {
        Ok((field.write)(self.record(key)?).get_or_insert_with(create))
    }

    pub(crate) fn remove<V>(&mut self, field: Field<R, V>, key: &CallKey) -> Option<V> {
        let Entry::Occupied(mut slot) = self.0.entry(key.handle().slot()) else {
            return None;
        };
        let (current, row) = slot.get_mut();
        if current != key {
            return None;
        }
        let value = (field.write)(row).take();
        if row.empty() {
            slot.remove();
        }
        value
    }

    pub(crate) fn iter<'a, V: 'a>(
        &'a self,
        field: Field<R, V>,
    ) -> impl Iterator<Item = (&'a CallKey, &'a V)> {
        self.0
            .values()
            .filter_map(move |(key, row)| Some((key, (field.read)(row).as_ref()?)))
    }

    pub(crate) fn retain<V>(
        &mut self,
        field: Field<R, V>,
        mut keep: impl FnMut(&CallKey, &mut V) -> bool,
    ) {
        self.0.retain(|_, (key, row)| {
            let value = (field.write)(row);
            if value.as_mut().is_some_and(|value| !keep(key, value)) {
                *value = None;
            }
            !row.empty()
        });
    }

    pub(crate) fn drain<V>(&mut self, field: Field<R, V>) -> Vec<(CallKey, V)> {
        let mut values = Vec::new();
        self.0.retain(|_, (key, row)| {
            if let Some(value) = (field.write)(row).take() {
                values.push((key.clone(), value));
            }
            !row.empty()
        });
        values
    }
}
