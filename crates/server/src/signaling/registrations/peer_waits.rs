use crate::ControlError;
use std::{net::SocketAddr, num::NonZeroU32};

#[derive(Clone, Copy)]
enum Slot {
    Free,
    Reserved(SocketAddr),
    Waiting { peer: SocketAddr, at: u64 },
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Admission {
    Ready,
    Wait { at: u64 },
}

pub(super) struct PeerWaits {
    slots: [Slot; 8],
}

impl Default for PeerWaits {
    fn default() -> Self {
        Self {
            slots: [Slot::Free; 8],
        }
    }
}

impl PeerWaits {
    pub(super) fn reserve(
        &mut self,
        peer: SocketAddr,
        now: u64,
    ) -> Result<Admission, ControlError> {
        for slot in &mut self.slots {
            if matches!(slot, Slot::Waiting { at, .. } if *at <= now) {
                *slot = Slot::Free;
            }
        }
        for slot in &self.slots {
            match slot {
                Slot::Reserved(reserved) if *reserved == peer => return Ok(Admission::Ready),
                Slot::Waiting { peer: waiting, at } if *waiting == peer => {
                    return Ok(Admission::Wait { at: *at });
                }
                Slot::Free | Slot::Reserved(_) | Slot::Waiting { .. } => {}
            }
        }
        if let Some(slot) = self
            .slots
            .iter_mut()
            .find(|slot| matches!(slot, Slot::Free))
        {
            *slot = Slot::Reserved(peer);
            return Ok(Admission::Ready);
        }
        let at = self
            .slots
            .iter()
            .filter_map(|slot| match slot {
                Slot::Waiting { at, .. } => Some(*at),
                Slot::Free | Slot::Reserved(_) => None,
            })
            .min()
            .ok_or(ControlError::Capacity)?;
        Ok(Admission::Wait { at })
    }

    pub(super) fn postpone(
        &mut self,
        peer: SocketAddr,
        interval: NonZeroU32,
        now: u64,
    ) -> Result<(), ControlError> {
        let at = now
            .checked_add(u64::from(interval.get()) * 1000)
            .ok_or(ControlError::Capacity)?;
        let slot = self
            .slots
            .iter_mut()
            .find(|slot| matches!(slot, Slot::Reserved(reserved) if *reserved == peer))
            .ok_or(ControlError::Invalid("Peer retry has no reservation"))?;
        *slot = Slot::Waiting { peer, at };
        Ok(())
    }

    pub(super) fn deadline(&self, peer: SocketAddr, now: u64) -> Option<u64> {
        self.slots.iter().find_map(|slot| match slot {
            Slot::Waiting { peer: waiting, at } if *waiting == peer && *at > now => Some(*at),
            Slot::Free | Slot::Reserved(_) | Slot::Waiting { .. } => None,
        })
    }

    pub(super) fn release_unused(&mut self) {
        for slot in &mut self.slots {
            if matches!(slot, Slot::Reserved(_)) {
                *slot = Slot::Free;
            }
        }
    }
}
