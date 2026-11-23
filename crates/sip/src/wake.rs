use crate::Engine;
use rusterisk_core::Wake;

/// The entries that an engine keeps until an input or an instant removes them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Retained {
    pub transactions: usize,
    pub replies: usize,
    pub outgoing: usize,
    pub challenges: usize,
    pub incoming: usize,
    pub forks: usize,
    pub refers: usize,
    pub contacts: usize,
}

impl Engine {
    /// When `tick` next changes the engine, on the clock of the `now` values that its owner passes.
    pub fn wake(&self) -> Wake {
        self.transactions
            .wake()
            .earliest(self.forks.wake())
            .earliest(self.refers.wake())
            .earliest(self.incoming_wake())
            .earliest(self.outgoing_wake())
            .earliest(self.contacts.wake())
    }

    pub fn retained(&self) -> Retained {
        Retained {
            transactions: self.transactions.len(),
            replies: self.cache.len(),
            outgoing: self.outgoing.len(),
            challenges: self.outgoing_challenges(),
            incoming: self.incoming.len(),
            forks: self.forks.len(),
            refers: self.refers.len(),
            contacts: self.contacts.len(),
        }
    }
}
