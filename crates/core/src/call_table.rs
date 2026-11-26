#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct CallHandle(u64);

impl CallHandle {
    pub const fn new(generation: u64) -> Self {
        Self(generation)
    }

    pub const fn slot(self) -> u8 {
        (self.0.wrapping_sub(1) % 64) as u8
    }

    pub const fn generation(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Slot {
    Vacant(u64),
    Live(CallHandle),
    Ending(CallHandle),
    AwaitingMedia(CallHandle),
}

pub struct CallTable<const N: usize> {
    slots: [Slot; N],
}

impl<const N: usize> Default for CallTable<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> CallTable<N> {
    pub const fn new() -> Self {
        Self {
            slots: [Slot::Vacant(0); N],
        }
    }

    pub fn available(&self, handle: CallHandle) -> bool {
        matches!(self.slots.get(usize::from(handle.slot())), Some(Slot::Vacant(last)) if handle.0 > *last)
    }

    pub fn admit(&mut self, handle: CallHandle) -> bool {
        let Some(slot) = self.slots.get_mut(usize::from(handle.slot())) else {
            return false;
        };
        if !matches!(*slot, Slot::Vacant(last) if handle.0 > last) {
            return false;
        }
        *slot = Slot::Live(handle);
        true
    }

    pub fn teardown(&mut self, handle: CallHandle) -> Option<Teardown> {
        let slot = self.slots.get_mut(usize::from(handle.slot()))?;
        if *slot != Slot::Live(handle) {
            return None;
        }
        *slot = Slot::Ending(handle);
        Some(Teardown {
            handle,
            next: Some(TeardownStep::Admissions),
        })
    }

    pub fn await_media(&mut self, teardown: Teardown) -> bool {
        let Some(slot) = self.slots.get_mut(usize::from(teardown.handle.slot())) else {
            return false;
        };
        if teardown.next.is_some() || *slot != Slot::Ending(teardown.handle) {
            return false;
        }
        *slot = Slot::AwaitingMedia(teardown.handle);
        true
    }

    pub fn acknowledge(&mut self, handle: CallHandle) -> bool {
        let Some(slot) = self.slots.get_mut(usize::from(handle.slot())) else {
            return false;
        };
        if *slot != Slot::AwaitingMedia(handle) {
            return false;
        }
        *slot = Slot::Vacant(handle.0);
        true
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TeardownStep {
    Admissions,
    Participants,
    Recordings,
    Dial,
    Bridge,
    Media,
}

pub struct Teardown {
    handle: CallHandle,
    next: Option<TeardownStep>,
}

impl Iterator for Teardown {
    type Item = TeardownStep;

    fn next(&mut self) -> Option<TeardownStep> {
        let step = self.next?;
        self.next = match step {
            TeardownStep::Admissions => Some(TeardownStep::Participants),
            TeardownStep::Participants => Some(TeardownStep::Recordings),
            TeardownStep::Recordings => Some(TeardownStep::Dial),
            TeardownStep::Dial => Some(TeardownStep::Bridge),
            TeardownStep::Bridge => Some(TeardownStep::Media),
            TeardownStep::Media => None,
        };
        Some(step)
    }
}
