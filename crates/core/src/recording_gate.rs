#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordingControl {
    Pause,
    Resume,
    Mute,
    Unmute,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordingGate {
    Open,
    Silent,
    Paused,
    PausedSilent,
}

impl RecordingGate {
    pub const fn apply(self, operation: RecordingControl) -> Self {
        match (self, operation) {
            (Self::Open | Self::Paused, RecordingControl::Pause) => Self::Paused,
            (Self::Silent | Self::PausedSilent, RecordingControl::Pause) => Self::PausedSilent,
            (Self::Open | Self::Paused, RecordingControl::Resume) => Self::Open,
            (Self::Silent | Self::PausedSilent, RecordingControl::Resume) => Self::Silent,
            (Self::Open | Self::Silent, RecordingControl::Mute) => Self::Silent,
            (Self::Paused | Self::PausedSilent, RecordingControl::Mute) => Self::PausedSilent,
            (Self::Open | Self::Silent, RecordingControl::Unmute) => Self::Open,
            (Self::Paused | Self::PausedSilent, RecordingControl::Unmute) => Self::Paused,
        }
    }
}
