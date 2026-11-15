use crate::{AudioSocket, MediaError, MAX_SAMPLES};

pub struct AudioSocketDecoder {
    bytes: [u8; MAX_SAMPLES * 2 + 3],
    filled: usize,
}

impl Default for AudioSocketDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioSocketDecoder {
    pub const fn new() -> Self {
        Self {
            bytes: [0; MAX_SAMPLES * 2 + 3],
            filled: 0,
        }
    }

    pub const fn is_empty(&self) -> bool {
        self.filled == 0
    }

    pub fn push(&mut self, input: &[u8]) -> Result<(usize, Option<AudioSocket>), MediaError> {
        let mut consumed = 0;
        while consumed < input.len() {
            let target = if self.filled < 3 {
                3
            } else {
                3 + usize::from(u16::from_be_bytes([self.bytes[1], self.bytes[2]]))
            };
            if target > self.bytes.len() {
                return Err(MediaError::Capacity);
            }
            let count = (target - self.filled).min(input.len() - consumed);
            self.bytes[self.filled..self.filled + count]
                .copy_from_slice(&input[consumed..consumed + count]);
            self.filled += count;
            consumed += count;
            if self.filled >= 3 {
                let length = 3 + usize::from(u16::from_be_bytes([self.bytes[1], self.bytes[2]]));
                if length > self.bytes.len() {
                    return Err(MediaError::Capacity);
                }
                if self.filled == length {
                    let message = AudioSocket::parse(&self.bytes[..length])?;
                    self.filled = 0;
                    return Ok((consumed, Some(message)));
                }
            }
        }
        Ok((consumed, None))
    }
}
