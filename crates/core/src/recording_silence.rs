use crate::RecordingGate;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecordingSilence {
    maximum_milliseconds: u64,
    phase: Phase,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Active {
        raw: u64,
        paused: u64,
        effective: u64,
    },
    Expired {
        trailing: u64,
    },
}

impl RecordingSilence {
    pub const fn new(maximum_seconds: u32) -> Self {
        Self {
            maximum_milliseconds: maximum_seconds as u64 * 1000,
            phase: Phase::Active {
                raw: 0,
                paused: 0,
                effective: 0,
            },
        }
    }

    pub fn observe(&mut self, milliseconds: u64, silent: bool, gate: RecordingGate) -> bool {
        match &mut self.phase {
            Phase::Expired { .. } => true,
            Phase::Active {
                raw,
                paused,
                effective,
            } => {
                if self.maximum_milliseconds == 0 {
                    return false;
                }
                *raw = if silent {
                    raw.saturating_add(milliseconds)
                } else {
                    0
                };
                *effective = match gate {
                    RecordingGate::Paused | RecordingGate::PausedSilent => {
                        *paused = *raw;
                        *raw
                    }
                    RecordingGate::Open | RecordingGate::Silent => {
                        if *raw > *paused {
                            *raw - *paused
                        } else {
                            *paused = 0;
                            *raw
                        }
                    }
                };
                if *effective > self.maximum_milliseconds {
                    self.phase = Phase::Expired {
                        trailing: *effective,
                    };
                    true
                } else {
                    false
                }
            }
        }
    }

    pub const fn trailing_milliseconds(self) -> u64 {
        match self.phase {
            Phase::Active { effective, .. } => effective,
            Phase::Expired { trailing } => trailing,
        }
    }
}
