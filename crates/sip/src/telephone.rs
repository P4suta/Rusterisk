use crate::SipError;
use rusterisk_core::DtmfDigit;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DtmfFormat {
    payload_type: u8,
    rate: u32,
    events: u16,
}

impl DtmfFormat {
    pub fn new(payload_type: u8, rate: u32) -> Result<Self, SipError> {
        if payload_type > 127 || !matches!(rate, 8000 | 48000) {
            return Err(SipError::Unsupported);
        }
        Ok(Self {
            payload_type,
            rate,
            events: u16::MAX,
        })
    }

    pub const fn payload_type(self) -> u8 {
        self.payload_type
    }
    pub const fn rate(self) -> u32 {
        self.rate
    }
    pub const fn events(self) -> u16 {
        self.events
    }
    pub const fn supports(self, digit: DtmfDigit) -> bool {
        self.events & (1 << digit.event()) != 0
    }

    pub(crate) fn with_parameters(mut self, value: &str) -> Result<Self, SipError> {
        self.events = 0;
        for part in value.split(',') {
            let (first, last) = part.split_once('-').unwrap_or((part, part));
            let first: u8 = first.parse().map_err(|_| SipError::InvalidPacket)?;
            let last: u8 = last.parse().map_err(|_| SipError::InvalidPacket)?;
            if first > last {
                return Err(SipError::InvalidPacket);
            }
            for event in first..=last.min(15) {
                self.events |= 1 << event;
            }
        }
        Ok(self)
    }

    pub(crate) fn parameters(self) -> String {
        let mut ranges = Vec::with_capacity(8);
        let mut event = 0u8;
        while event < 16 {
            if self.events & (1 << event) == 0 {
                event += 1;
                continue;
            }
            let first = event;
            while event < 15 && self.events & (1 << (event + 1)) != 0 {
                event += 1;
            }
            ranges.push(if first == event {
                first.to_string()
            } else {
                format!("{first}-{event}")
            });
            event += 1;
        }
        ranges.join(",")
    }
}
