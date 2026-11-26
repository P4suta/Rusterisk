use crate::{Frame, MediaError, MAX_SAMPLES};
use std::collections::VecDeque;

pub struct PcmFramer {
    samples: VecDeque<i16>,
    rate: u32,
    limit: usize,
}

impl PcmFramer {
    pub fn new(rate: u32) -> Result<Self, MediaError> {
        Self::with_capacity(rate, 10)
    }

    pub(crate) fn with_capacity(rate: u32, frames: usize) -> Result<Self, MediaError> {
        Frame::new(&[0], rate)?;
        let limit = (frames + 1) * (rate as usize / 50) - 1;
        Ok(Self {
            samples: VecDeque::with_capacity(limit),
            rate,
            limit,
        })
    }

    pub(crate) fn rate(&self) -> u32 {
        self.rate
    }

    pub fn push(&mut self, frame: Frame) -> Result<(), MediaError> {
        if frame.rate() != self.rate {
            return Err(MediaError::InvalidFrame);
        }
        self.push_samples(frame.samples())
    }

    pub(crate) fn push_samples(&mut self, samples: &[i16]) -> Result<(), MediaError> {
        if !self.can_push(samples.len()) {
            return Err(MediaError::Capacity);
        }
        self.samples.extend(samples.iter().copied());
        Ok(())
    }

    pub(crate) fn can_push(&self, samples: usize) -> bool {
        samples <= self.limit - self.samples.len()
    }

    pub fn pop(&mut self) -> Option<Frame> {
        let len = self.rate as usize / 50;
        if self.samples.len() < len {
            return None;
        }
        let mut samples = [0; MAX_SAMPLES];
        let (front, back) = self.samples.as_slices();
        let first = len.min(front.len());
        samples[..first].copy_from_slice(&front[..first]);
        samples[first..len].copy_from_slice(&back[..len - first]);
        self.samples.drain(..len);
        Some(Frame {
            samples,
            len,
            rate: self.rate,
        })
    }

    pub(crate) fn discard_partial(&mut self) {
        let len = self.rate as usize / 50;
        self.samples.truncate(self.samples.len() / len * len);
    }

    pub(crate) fn reserved_bytes(&self) -> usize {
        self.samples.capacity() * size_of::<i16>()
    }

    pub fn clear(&mut self) {
        self.samples.clear();
    }
}
