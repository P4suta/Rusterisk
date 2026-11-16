use super::RegistrationResponse;
use crate::{SipError, MAX_MESSAGE};
use std::{net::Ipv6Addr, num::NonZeroU32};

const MAX_PARAMETERS: usize = 16;

impl RegistrationResponse {
    pub fn retry_after(&self) -> Result<Option<NonZeroU32>, SipError> {
        let mut values = self.header_values("Retry-After");
        let Some(value) = values.next() else {
            return Ok(None);
        };
        if values.next().is_some() {
            return Err(SipError::InvalidPacket);
        }
        Parser {
            source: value.as_bytes(),
            position: 0,
        }
        .parse()
    }
}

struct Parser<'a> {
    source: &'a [u8],
    position: usize,
}

impl<'a> Parser<'a> {
    fn parse(mut self) -> Result<Option<NonZeroU32>, SipError> {
        if self.source.len() > MAX_MESSAGE {
            return Err(SipError::Capacity);
        }
        self.whitespace();
        let seconds = self.number()?;
        self.whitespace();
        if self.peek() == Some(b'(') {
            self.comment()?;
            self.whitespace();
        }
        let mut parameters: Vec<&[u8]> = Vec::with_capacity(MAX_PARAMETERS);
        while self.peek().is_some() {
            if self.take() != Some(b';') {
                return Err(SipError::InvalidPacket);
            }
            self.whitespace();
            let name = self.token()?;
            if parameters
                .iter()
                .any(|previous| previous.eq_ignore_ascii_case(name))
            {
                return Err(SipError::InvalidPacket);
            }
            if parameters.len() == MAX_PARAMETERS {
                return Err(SipError::Capacity);
            }
            parameters.push(name);
            self.whitespace();
            if self.peek() == Some(b'=') {
                self.position += 1;
                self.whitespace();
                if name.eq_ignore_ascii_case(b"duration") {
                    self.number()?;
                } else {
                    self.value()?;
                }
            } else if name.eq_ignore_ascii_case(b"duration") {
                return Err(SipError::InvalidPacket);
            }
            self.whitespace();
        }
        Ok(NonZeroU32::new(seconds))
    }

    fn peek(&self) -> Option<u8> {
        self.source.get(self.position).copied()
    }

    fn take(&mut self) -> Option<u8> {
        let byte = self.peek()?;
        self.position += 1;
        Some(byte)
    }

    fn whitespace(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t')) {
            self.position += 1;
        }
    }

    fn number(&mut self) -> Result<u32, SipError> {
        let start = self.position;
        let mut value = 0_u32;
        while let Some(byte) = self.peek().filter(u8::is_ascii_digit) {
            value = value
                .checked_mul(10)
                .and_then(|value| value.checked_add(u32::from(byte - b'0')))
                .ok_or(SipError::InvalidPacket)?;
            self.position += 1;
        }
        if self.position == start {
            return Err(SipError::InvalidPacket);
        }
        Ok(value)
    }

    fn token(&mut self) -> Result<&'a [u8], SipError> {
        let start = self.position;
        while self
            .peek()
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || b"-.!%*_+`'~".contains(&byte))
        {
            self.position += 1;
        }
        if self.position == start {
            return Err(SipError::InvalidPacket);
        }
        Ok(&self.source[start..self.position])
    }

    fn value(&mut self) -> Result<(), SipError> {
        match self.peek() {
            Some(b'"') => self.quoted(),
            Some(b'[') => {
                self.position += 1;
                let start = self.position;
                while self.peek().is_some_and(|byte| byte != b']') {
                    self.position += 1;
                }
                std::str::from_utf8(&self.source[start..self.position])?
                    .parse::<Ipv6Addr>()
                    .map_err(|_| SipError::InvalidPacket)?;
                if self.take() != Some(b']') {
                    return Err(SipError::InvalidPacket);
                }
                Ok(())
            }
            Some(_) | None => self.token().map(|_token| ()),
        }
    }

    fn escaped(&mut self) -> Result<(), SipError> {
        match self.take() {
            Some(byte) if byte.is_ascii() && !matches!(byte, b'\r' | b'\n') => Ok(()),
            Some(_) | None => Err(SipError::InvalidPacket),
        }
    }

    fn quoted(&mut self) -> Result<(), SipError> {
        self.position += 1;
        while let Some(byte) = self.take() {
            match byte {
                b'"' => return Ok(()),
                b'\\' => self.escaped()?,
                b'\t' | b' '..=b'~' | 0x80..=0xff => {}
                0..=8 | 10..=31 | 0x7f => return Err(SipError::InvalidPacket),
            }
        }
        Err(SipError::InvalidPacket)
    }

    fn comment(&mut self) -> Result<(), SipError> {
        self.position += 1;
        let mut depth = 1_usize;
        while let Some(byte) = self.take() {
            match byte {
                b'(' => depth = depth.checked_add(1).ok_or(SipError::Capacity)?,
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        return Ok(());
                    }
                }
                b'\\' => self.escaped()?,
                b'\t' | b' '..=b'~' | 0x80..=0xff => {}
                0..=8 | 10..=31 | 0x7f => return Err(SipError::InvalidPacket),
            }
        }
        Err(SipError::InvalidPacket)
    }
}
