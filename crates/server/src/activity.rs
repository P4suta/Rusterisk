use rusterisk_api::diagnosis::{CallSlots, LoopActivity, RuntimeActivity};
use rusterisk_core::Wake;
use tokio::sync::watch;

#[derive(Clone)]
pub(crate) struct Meter(watch::Sender<LoopActivity>);

impl Default for Meter {
    fn default() -> Self {
        Self(watch::channel(LoopActivity::default()).0)
    }
}

impl Meter {
    pub(crate) fn turn(&self, late: bool, drops: u64) {
        self.0.send_modify(|activity| {
            activity.turns = activity.turns.saturating_add(1);
            activity.late_turns = activity.late_turns.saturating_add(u64::from(late));
            activity.drops = activity.drops.saturating_add(drops);
        });
    }

    pub(crate) fn dropped(&self) {
        self.0
            .send_modify(|activity| activity.drops = activity.drops.saturating_add(1));
    }

    pub(crate) fn deadline(&self, wake: Wake, now: u64) {
        self.turn(matches!(wake, Wake::At(at) if at != 0 && now > at), 0);
    }
}

#[derive(Clone, Default)]
pub(crate) struct Meters {
    pub(crate) control: Meter,
    pub(crate) signaling: Meter,
    pub(crate) media: Meter,
}

impl Meters {
    pub(crate) fn snapshot(&self, call_slots: CallSlots) -> RuntimeActivity {
        RuntimeActivity {
            call_slots,
            control: *self.control.0.borrow(),
            signaling: *self.signaling.0.borrow(),
            media: *self.media.0.borrow(),
        }
    }
}
