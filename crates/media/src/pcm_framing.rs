use crate::{Frame, MediaError, MAX_SAMPLES};
use std::collections::VecDeque;

pub struct PcmFramer {
    partial: [i16; MAX_SAMPLES],
    filled: usize,
    rate: u32,
    ready: VecDeque<Frame>,
}

impl PcmFramer {
    pub fn new(rate: u32) -> Result<Self, MediaError> {
        Frame::new(&[0], rate)?;
        Ok(Self {
            partial: [0; MAX_SAMPLES],
            filled: 0,
            rate,
            ready: VecDeque::with_capacity(10),
        })
    }

    pub fn push(&mut self, frame: Frame) -> Result<(), MediaError> {
        if frame.rate() != self.rate {
            return Err(MediaError::InvalidFrame);
        }
        let frame_len = self.rate as usize / 50;
        if !self.can_push(frame.samples().len()) {
            return Err(MediaError::Capacity);
        }
        for sample in frame.samples() {
            self.partial[self.filled] = *sample;
            self.filled += 1;
            if self.filled == frame_len {
                self.ready
                    .push_back(Frame::new(&self.partial[..frame_len], self.rate)?);
                self.filled = 0;
            }
        }
        Ok(())
    }

    pub(crate) fn can_push(&self, samples: usize) -> bool {
        self.filled
            .checked_add(samples)
            .and_then(|total| {
                self.ready
                    .len()
                    .checked_add(total / (self.rate as usize / 50))
            })
            .is_some_and(|frames| frames <= 10)
    }

    pub fn pop(&mut self) -> Option<Frame> {
        self.ready.pop_front()
    }

    pub fn clear(&mut self) {
        self.filled = 0;
        self.ready.clear();
    }
}
