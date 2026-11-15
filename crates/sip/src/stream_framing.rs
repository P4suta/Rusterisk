use crate::MAX_MESSAGE;
use std::ops::Range;

#[derive(Debug, PartialEq, Eq)]
pub enum StreamItem {
    Message(Vec<u8>),
    Crlf,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamFramingError {
    InvalidLine,
    InvalidLength,
    DuplicateLength,
    MissingLength,
    Capacity,
    Incomplete,
    Closed,
}

impl std::fmt::Display for StreamFramingError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::InvalidLine => "invalid stream header line",
            Self::InvalidLength => "invalid stream body length",
            Self::DuplicateLength => "duplicate stream body length",
            Self::MissingLength => "missing stream body length",
            Self::Capacity => "stream message exceeds the packet limit",
            Self::Incomplete => "stream closed during a message",
            Self::Closed => "stream decoder is closed",
        })
    }
}

impl std::error::Error for StreamFramingError {}

enum Phase {
    Headers,
    Body { total: usize },
    Closed,
}

pub struct SipStreamDecoder {
    bytes: [u8; MAX_MESSAGE],
    filled: usize,
    phase: Phase,
}

impl Default for SipStreamDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl SipStreamDecoder {
    pub const fn new() -> Self {
        Self {
            bytes: [0; MAX_MESSAGE],
            filled: 0,
            phase: Phase::Headers,
        }
    }

    pub const fn is_empty(&self) -> bool {
        self.filled == 0
    }

    /// Return one complete message or leading `CRLF` pair and the consumed input count.
    /// Keep unconsumed input for the next call and preserve the decoder across read cancellation.
    /// Every framing error closes the decoder.
    pub fn push(
        &mut self,
        input: &[u8],
    ) -> Result<(usize, Option<StreamItem>), StreamFramingError> {
        let result = self.consume(input);
        if result.is_err() {
            self.phase = Phase::Closed;
            self.filled = 0;
        }
        result
    }

    fn complete(&mut self) -> StreamItem {
        let message = StreamItem::Message(self.bytes[..self.filled].to_vec());
        self.filled = 0;
        self.phase = Phase::Headers;
        message
    }

    fn consume(&mut self, input: &[u8]) -> Result<(usize, Option<StreamItem>), StreamFramingError> {
        if matches!(self.phase, Phase::Closed) {
            return Err(StreamFramingError::Closed);
        }
        let mut consumed = 0;
        while consumed < input.len() {
            match self.phase {
                Phase::Headers => {
                    if self.filled == MAX_MESSAGE {
                        return Err(StreamFramingError::Capacity);
                    }
                    let byte = input[consumed];
                    let previous = self.filled.checked_sub(1).map(|index| self.bytes[index]);
                    if (byte == b'\n' && previous != Some(b'\r'))
                        || (previous == Some(b'\r') && byte != b'\n')
                    {
                        return Err(StreamFramingError::InvalidLine);
                    }
                    self.bytes[self.filled] = byte;
                    self.filled += 1;
                    consumed += 1;
                    if self.filled == 2 && self.bytes[..2] == *b"\r\n" {
                        self.filled = 0;
                        return Ok((consumed, Some(StreamItem::Crlf)));
                    }
                    if self.filled >= 4 && self.bytes[self.filled - 4..self.filled] == *b"\r\n\r\n"
                    {
                        let body = body_length(&self.bytes[..self.filled - 4])?;
                        let total = self
                            .filled
                            .checked_add(body)
                            .filter(|total| *total <= MAX_MESSAGE)
                            .ok_or(StreamFramingError::Capacity)?;
                        if body == 0 {
                            return Ok((consumed, Some(self.complete())));
                        }
                        self.phase = Phase::Body { total };
                    } else if self.filled == MAX_MESSAGE {
                        return Err(StreamFramingError::Capacity);
                    }
                }
                Phase::Body { total } => {
                    let count = (total - self.filled).min(input.len() - consumed);
                    self.bytes[self.filled..self.filled + count]
                        .copy_from_slice(&input[consumed..consumed + count]);
                    self.filled += count;
                    consumed += count;
                    if self.filled == total {
                        return Ok((consumed, Some(self.complete())));
                    }
                }
                Phase::Closed => return Err(StreamFramingError::Closed),
            }
        }
        Ok((consumed, None))
    }

    pub fn finish(&mut self) -> Result<(), StreamFramingError> {
        let result = match self.phase {
            Phase::Headers if self.filled == 0 => Ok(()),
            Phase::Headers | Phase::Body { .. } => Err(StreamFramingError::Incomplete),
            Phase::Closed => Err(StreamFramingError::Closed),
        };
        self.phase = Phase::Closed;
        self.filled = 0;
        result
    }
}

fn trim_spaces(bytes: &[u8]) -> &[u8] {
    let first = bytes
        .iter()
        .position(|byte| !matches!(byte, b' ' | b'\t'))
        .unwrap_or(bytes.len());
    let end = bytes
        .iter()
        .rposition(|byte| !matches!(byte, b' ' | b'\t'))
        .map_or(first, |index| index + 1);
    &bytes[first..end]
}

fn header_token(bytes: &[u8]) -> bool {
    !bytes.is_empty()
        && bytes.iter().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'-' | b'.' | b'!' | b'%' | b'*' | b'_' | b'+' | b'`' | b'\'' | b'~'
                )
        })
}

enum HeaderKind {
    Length,
    Other,
}

pub(super) fn body_length(headers: &[u8]) -> Result<usize, StreamFramingError> {
    let mut lines = headers.split(|byte| *byte == b'\n');
    let start = lines.next().ok_or(StreamFramingError::InvalidLine)?;
    if start.is_empty() || start.starts_with(b" ") || start.starts_with(b"\t") {
        return Err(StreamFramingError::InvalidLine);
    }
    let mut offset = start.len() + 1;
    let mut value: Option<Range<usize>> = None;
    let mut current = None;
    for raw in lines {
        let line = raw.strip_suffix(b"\r").unwrap_or(raw);
        if line.starts_with(b" ") || line.starts_with(b"\t") {
            match current {
                Some(HeaderKind::Length) => {
                    let range = value.as_mut().ok_or(StreamFramingError::InvalidLine)?;
                    range.end = offset + line.len();
                }
                Some(HeaderKind::Other) => {}
                None => return Err(StreamFramingError::InvalidLine),
            }
        } else {
            let colon = line
                .iter()
                .position(|byte| *byte == b':')
                .ok_or(StreamFramingError::InvalidLine)?;
            let name = trim_spaces(&line[..colon]);
            if !header_token(name) {
                return Err(StreamFramingError::InvalidLine);
            }
            let length_header =
                name.eq_ignore_ascii_case(b"Content-Length") || name.eq_ignore_ascii_case(b"l");
            current = Some(if length_header {
                HeaderKind::Length
            } else {
                HeaderKind::Other
            });
            if length_header {
                if value.is_some() {
                    return Err(StreamFramingError::DuplicateLength);
                }
                value = Some(offset + colon + 1..offset + line.len());
            }
        }
        offset += raw.len() + 1;
    }
    decimal_length(&headers[value.ok_or(StreamFramingError::MissingLength)?])
}

enum NumberPhase {
    Leading,
    Digits,
    Trailing,
}

fn decimal_length(bytes: &[u8]) -> Result<usize, StreamFramingError> {
    let mut phase = NumberPhase::Leading;
    let mut value = 0usize;
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            byte @ b'0'..=b'9' => {
                match phase {
                    NumberPhase::Leading | NumberPhase::Digits => phase = NumberPhase::Digits,
                    NumberPhase::Trailing => return Err(StreamFramingError::InvalidLength),
                }
                value = value
                    .checked_mul(10)
                    .and_then(|value| value.checked_add(usize::from(byte - b'0')))
                    .filter(|value| *value <= MAX_MESSAGE)
                    .ok_or(StreamFramingError::Capacity)?;
            }
            b' ' | b'\t' => match phase {
                NumberPhase::Digits => phase = NumberPhase::Trailing,
                NumberPhase::Leading | NumberPhase::Trailing => {}
            },
            b'\r'
                if bytes.get(index + 1) == Some(&b'\n')
                    && bytes
                        .get(index + 2)
                        .is_some_and(|byte| matches!(byte, b' ' | b'\t')) =>
            {
                if matches!(phase, NumberPhase::Digits) {
                    phase = NumberPhase::Trailing;
                }
                index += 1;
            }
            _ => return Err(StreamFramingError::InvalidLength),
        }
        index += 1;
    }
    match phase {
        NumberPhase::Leading => Err(StreamFramingError::InvalidLength),
        NumberPhase::Digits | NumberPhase::Trailing => Ok(value),
    }
}
