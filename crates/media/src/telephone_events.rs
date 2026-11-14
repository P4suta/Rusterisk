use crate::MediaError;
use rusterisk_core::DtmfDigit;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TelephoneEvent {
    digit: DtmfDigit,
    ended: bool,
    volume: u8,
    duration: u16,
}

impl TelephoneEvent {
    pub fn new(
        digit: DtmfDigit,
        ended: bool,
        volume: u8,
        duration: u16,
    ) -> Result<Self, MediaError> {
        if volume > 63 {
            return Err(MediaError::InvalidPacket);
        }
        Ok(Self {
            digit,
            ended,
            volume,
            duration,
        })
    }

    pub fn parse(bytes: &[u8]) -> Result<Self, MediaError> {
        let [digit, flags, high, low] = bytes else {
            return Err(MediaError::InvalidPacket);
        };
        Self::new(
            DtmfDigit::from_event(*digit).ok_or(MediaError::Unsupported)?,
            flags & 0x80 != 0,
            flags & 0x3f,
            u16::from_be_bytes([*high, *low]),
        )
    }

    pub const fn encode(self) -> [u8; 4] {
        let [high, low] = self.duration.to_be_bytes();
        [
            self.digit.event(),
            self.volume | if self.ended { 0x80 } else { 0 },
            high,
            low,
        ]
    }

    pub const fn digit(self) -> DtmfDigit {
        self.digit
    }
    pub const fn ended(self) -> bool {
        self.ended
    }
    pub const fn duration(self) -> u16 {
        self.duration
    }
}
