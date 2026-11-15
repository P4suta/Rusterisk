use crate::{stream_framing::body_length, StreamFramingError, MAX_MESSAGE};

pub fn validate_websocket_message(bytes: &[u8]) -> Result<(), StreamFramingError> {
    if bytes.len() > MAX_MESSAGE {
        return Err(StreamFramingError::Capacity);
    }
    let separator = bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or(StreamFramingError::Incomplete)?;
    let headers = &bytes[..separator];
    if std::str::from_utf8(headers).is_err() {
        return Err(StreamFramingError::InvalidLine);
    }
    for (index, byte) in headers.iter().enumerate() {
        let previous = index.checked_sub(1).map(|index| headers[index]);
        if (*byte == b'\n' && previous != Some(b'\r'))
            || (previous == Some(b'\r') && *byte != b'\n')
            || (*byte == b'\r' && index + 1 == headers.len())
        {
            return Err(StreamFramingError::InvalidLine);
        }
    }
    let available = bytes.len() - separator - 4;
    match body_length(headers) {
        Ok(length) if length == available => Ok(()),
        Ok(_) => Err(StreamFramingError::InvalidLength),
        Err(StreamFramingError::MissingLength) => Ok(()),
        Err(error) => Err(error),
    }
}
