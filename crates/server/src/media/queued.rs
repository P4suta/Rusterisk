use super::{Command, MediaHub, PlaybackControl};

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Queued {
    Reservation,
    Stop,
    Release,
    Other,
}

impl MediaHub {
    // Takes the next command from the queue without running it.
    pub(crate) fn take_queued(&mut self) -> Option<Queued> {
        let command = self.commands.try_recv().ok()?;
        let reservation = matches!(
            command,
            Command::ReservePlayback { .. }
                | Command::ReserveMusic { .. }
                | Command::ReserveRecording { .. }
        );
        let stop = matches!(
            command,
            Command::ControlPlayback {
                operation: PlaybackControl::Stop,
                ..
            } | Command::EndRecording { .. }
        );
        Some(if reservation {
            Queued::Reservation
        } else if stop {
            Queued::Stop
        } else if matches!(command, Command::Release { .. }) {
            Queued::Release
        } else {
            Queued::Other
        })
    }
}
