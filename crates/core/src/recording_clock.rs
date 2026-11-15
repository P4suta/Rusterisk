use crate::RecordingGate;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecordingClock {
    limit_seconds: u32,
    phase: Phase,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Active { milliseconds: u64 },
    Expired,
}

impl RecordingClock {
    pub const fn new(limit_seconds: u32) -> Self {
        Self {
            limit_seconds,
            phase: Phase::Active { milliseconds: 0 },
        }
    }

    pub fn advance(&mut self, milliseconds: u64, gate: RecordingGate) -> bool {
        match &mut self.phase {
            Phase::Expired => true,
            Phase::Active {
                milliseconds: elapsed,
            } => {
                if self.limit_seconds == 0 {
                    return false;
                }
                match gate {
                    RecordingGate::Paused | RecordingGate::PausedSilent => false,
                    RecordingGate::Open | RecordingGate::Silent => {
                        let deadline = (u64::from(self.limit_seconds) + 1) * 1000;
                        *elapsed = elapsed.saturating_add(milliseconds).min(deadline);
                        if *elapsed == deadline {
                            self.phase = Phase::Expired;
                            true
                        } else {
                            false
                        }
                    }
                }
            }
        }
    }
}
