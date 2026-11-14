use crate::DtmfDigit;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DtmfError {
    Empty,
    Capacity,
    Timing,
    Clock,
    Unsupported,
    Ended,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DtmfTiming {
    before: u32,
    between: u32,
    duration: u32,
    after: u32,
}

impl DtmfTiming {
    pub const fn new(
        before: u32,
        between: u32,
        duration: u32,
        after: u32,
    ) -> Result<Self, DtmfError> {
        if before > 5000 || between > 5000 || duration == 0 || duration > 5000 || after > 5000 {
            return Err(DtmfError::Timing);
        }
        Ok(Self {
            before,
            between,
            duration,
            after,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DtmfTone {
    pub digit: DtmfDigit,
    pub duration_ms: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Pending,
    Waiting(u64),
    Completing(u64),
    Ended,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DtmfSequence {
    digits: [DtmfDigit; 32],
    len: usize,
    index: usize,
    timing: DtmfTiming,
    phase: Phase,
}

impl DtmfSequence {
    pub fn new(digits: &[DtmfDigit], timing: DtmfTiming) -> Result<Self, DtmfError> {
        if digits.is_empty() {
            return Err(DtmfError::Empty);
        }
        if digits.len() > 32 {
            return Err(DtmfError::Capacity);
        }
        let mut owned = [DtmfDigit::Zero; 32];
        owned[..digits.len()].copy_from_slice(digits);
        Ok(Self {
            digits: owned,
            len: digits.len(),
            index: 0,
            timing,
            phase: Phase::Pending,
        })
    }

    pub fn cancel(&mut self) {
        self.phase = Phase::Ended;
    }

    pub const fn finished(&self) -> bool {
        matches!(self.phase, Phase::Ended)
    }

    pub fn advance(&mut self, now: u64) -> Result<Option<DtmfTone>, DtmfError> {
        let phase = match self.phase {
            Phase::Pending => Phase::Waiting(
                now.checked_add(u64::from(self.timing.before))
                    .ok_or(DtmfError::Clock)?,
            ),
            Phase::Waiting(deadline) => Phase::Waiting(deadline),
            Phase::Completing(deadline) if now >= deadline => Phase::Ended,
            Phase::Completing(deadline) => Phase::Completing(deadline),
            Phase::Ended => Phase::Ended,
        };
        if let Phase::Waiting(deadline) = phase {
            if now >= deadline {
                let pause = if self.index + 1 == self.len {
                    self.timing.after
                } else {
                    self.timing.between
                };
                let deadline = now
                    .checked_add(u64::from(self.timing.duration) + u64::from(pause))
                    .ok_or(DtmfError::Clock)?;
                let tone = DtmfTone {
                    digit: self.digits[self.index],
                    duration_ms: self.timing.duration,
                };
                self.index += 1;
                self.phase = if self.index == self.len {
                    Phase::Completing(deadline)
                } else {
                    Phase::Waiting(deadline)
                };
                return Ok(Some(tone));
            }
        }
        self.phase = phase;
        Ok(None)
    }
}
