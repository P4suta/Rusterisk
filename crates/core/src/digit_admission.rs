use crate::{DtmfDigit, DtmfError, DtmfSequence, DtmfTiming};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DtmfToken(u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DtmfCommand {
    pub token: DtmfToken,
    pub sequence: DtmfSequence,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Admission {
    Ready(u16),
    Ended,
}

pub struct DtmfOutbox {
    state: Admission,
    tokens: [Option<DtmfToken>; 8],
    issued: u64,
}

impl DtmfOutbox {
    pub const fn new(events: u16) -> Result<Self, DtmfError> {
        if events == 0 {
            return Err(DtmfError::Unsupported);
        }
        Ok(Self {
            state: Admission::Ready(events),
            tokens: [None; 8],
            issued: 0,
        })
    }

    pub fn admit(
        &mut self,
        digits: &[DtmfDigit],
        timing: DtmfTiming,
    ) -> Result<DtmfCommand, DtmfError> {
        let events = match self.state {
            Admission::Ready(events) => events,
            Admission::Ended => return Err(DtmfError::Ended),
        };
        let sequence = DtmfSequence::new(digits, timing)?;
        if digits
            .iter()
            .any(|digit| events & (1 << digit.event()) == 0)
        {
            return Err(DtmfError::Unsupported);
        }
        let slot = self
            .tokens
            .iter()
            .position(Option::is_none)
            .ok_or(DtmfError::Capacity)?;
        let issued = self.issued.checked_add(1).ok_or(DtmfError::Capacity)?;
        let token = DtmfToken(issued);
        self.tokens[slot] = Some(token);
        self.issued = issued;
        Ok(DtmfCommand { token, sequence })
    }

    pub fn finish(&mut self, token: DtmfToken) -> bool {
        if let Some(slot) = self.tokens.iter_mut().find(|slot| **slot == Some(token)) {
            *slot = None;
            true
        } else {
            false
        }
    }

    pub fn cancel(&mut self) {
        self.state = Admission::Ended;
        self.tokens.fill(None);
    }
}
