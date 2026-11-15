use crate::{OracleError, MAX_INPUT};
use rusterisk_api::{ChannelId, MediaCommand, MediaControlFormat, MediaEvent};

pub fn controls(bytes: &[u8]) -> Result<usize, OracleError> {
    if bytes.len() > MAX_INPUT {
        return Err(OracleError::InputCapacity);
    }
    let Ok(source) = std::str::from_utf8(bytes) else {
        return Ok(0);
    };
    let channel = ChannelId::try_from("fuzzer".to_owned()).map_err(|_| OracleError::Channel)?;
    let mut accepted = 0;
    for format in [MediaControlFormat::PlainText, MediaControlFormat::Json] {
        if let Ok(command) = MediaCommand::parse(source, format) {
            let encoded = command
                .encode(format)
                .map_err(|_| OracleError::ControlEncoding)?;
            if !matches!(MediaCommand::parse(&encoded, format), Ok(parsed) if parsed == command) {
                return Err(OracleError::ControlChanged);
            }
            accepted += 1;
        }
        if let Ok(event) = MediaEvent::parse(source, format, &channel) {
            let encoded = event
                .encode(format)
                .map_err(|_| OracleError::ControlEncoding)?;
            if !matches!(MediaEvent::parse(&encoded, format, &channel), Ok(parsed) if parsed == event)
            {
                return Err(OracleError::ControlChanged);
            }
            accepted += 1;
        }
    }
    Ok(accepted)
}
