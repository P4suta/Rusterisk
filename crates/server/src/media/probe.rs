use super::{Command, MediaPort};
use crate::{CallKey, ControlError};
use rusterisk_media::{Frame, MediaError};
use std::f64::consts::TAU;
use tokio::sync::watch;

const START_TONE: u8 = 8;
const TONES: usize = 9;
const SYMBOLS: usize = 17;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Progress {
    Silent,
    OtherAudio,
    Verified,
}

pub(super) struct Challenge {
    tones: [u8; SYMBOLS],
    coefficients: [f64; TONES],
    sent: usize,
    matched: usize,
    last: Option<u8>,
    progress: watch::Sender<Progress>,
}

fn frequency(index: u8) -> f64 {
    400.0 + 100.0 * f64::from(index)
}

impl Challenge {
    fn new(seed: &[u8; 16], progress: watch::Sender<Progress>) -> Self {
        let mut previous = 0;
        let tones = std::array::from_fn(|index| {
            if index == 0 {
                return START_TONE;
            }
            let tone = if index == 1 {
                seed[index - 1] % 8
            } else {
                (previous + 1 + seed[index - 1] % 7) % 8
            };
            previous = tone;
            tone
        });
        let coefficients = std::array::from_fn(|i| 2.0 * (TAU * frequency(i as u8) / 8000.0).cos());
        Self {
            tones,
            coefficients,
            sent: 0,
            matched: 0,
            last: None,
            progress,
        }
    }
    pub(super) fn closed(&self) -> bool {
        self.progress.is_closed()
    }
    pub(super) fn transmit(&mut self) -> Result<Frame, MediaError> {
        let tone = self.tones[(self.sent / 3) % SYMBOLS];
        self.sent = self.sent.saturating_add(1);
        let samples: [i16; 160] = std::array::from_fn(|i| {
            (4500.0 * (TAU * frequency(tone) * i as f64 / 8000.0).sin()).round() as i16
        });
        Frame::new(&samples, 8000)?.resample(48000)
    }

    pub(super) fn receive(&mut self, frame: &Frame) -> Result<(), MediaError> {
        if self.matched == SYMBOLS || self.sent == 0 {
            return Ok(());
        }
        let pcm = frame.resample(8000)?;
        let samples = pcm.samples();
        let energy = samples.iter().map(|&x| f64::from(x).powi(2)).sum::<f64>();
        if energy < (samples.len() as f64) * 100.0_f64.powi(2) {
            return Ok(());
        }
        self.progress.send_if_modified(|progress| {
            if *progress == Progress::Silent {
                *progress = Progress::OtherAudio;
                true
            } else {
                false
            }
        });
        let mut strongest = None;
        let mut threshold = 0.32 * energy * samples.len() as f64;
        for (i, &coefficient) in self.coefficients.iter().enumerate() {
            let (mut previous, mut older) = (0.0, 0.0);
            for &sample in samples {
                let current = f64::from(sample) + coefficient * previous - older;
                older = previous;
                previous = current;
            }
            let power = previous * previous + older * older - coefficient * previous * older;
            if power > threshold {
                strongest = Some(i as u8);
                threshold = power;
            }
        }
        if let Some(tone) = strongest {
            if self.last != Some(tone) {
                self.last = Some(tone);
                if self.tones[self.matched] == tone {
                    self.matched += 1;
                } else {
                    self.matched = usize::from(tone == self.tones[0]);
                }
                if self.matched == SYMBOLS && self.sent > (SYMBOLS - 1) * 3 {
                    self.progress.send_replace(Progress::Verified);
                } else if self.matched == SYMBOLS {
                    self.matched = 0;
                }
            }
        }
        Ok(())
    }
}

impl MediaPort {
    pub(crate) fn test_signal(
        &self,
        key: CallKey,
    ) -> Result<watch::Receiver<Progress>, ControlError> {
        let (sender, receiver) = watch::channel(Progress::Silent);
        let challenge = Challenge::new(uuid::Uuid::new_v4().as_bytes(), sender);
        self.commands
            .try_send(Command::Probe { key, challenge })
            .map_err(|_| ControlError::Capacity)?;
        Ok(receiver)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_previous_call_signal_cannot_complete_the_current_challenge() -> Result<(), MediaError> {
        let (out, progress) = watch::channel(Progress::Silent);
        let (old, _old_progress) = watch::channel(Progress::Silent);
        let mut current = Challenge::new(&[13; 16], out);
        let mut previous = Challenge::new(&[27; 16], old);
        for _ in 0..80 {
            let _sent = current.transmit()?;
            current.receive(&previous.transmit()?)?;
        }
        assert_eq!(*progress.borrow(), Progress::OtherAudio);
        for _ in 0..100 {
            let signal = current.transmit()?;
            current.receive(&signal)?;
        }
        assert_eq!(*progress.borrow(), Progress::Verified);
        Ok(())
    }
}
