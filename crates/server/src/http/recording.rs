use super::{id, required};
use crate::ControlError;
use rusterisk_api::{ChannelId, RecordingIfExists};
use rusterisk_core::DtmfDigit;
use std::collections::BTreeMap;

pub(super) struct Arguments {
    pub name: ChannelId,
    pub policy: RecordingIfExists,
}

pub(super) fn arguments(parameters: &BTreeMap<String, String>) -> Result<Arguments, ControlError> {
    let name = id(required(parameters, "name")?)?;
    let format = required(parameters, "format")?;
    if format != "wav" {
        return Err(ControlError::Unprocessable(
            "Specified format is unknown on this system",
        ));
    }
    for option in ["maxDurationSeconds", "maxSilenceSeconds"] {
        let seconds = parameters
            .get(option)
            .map(|value| value.parse::<u32>())
            .transpose()
            .map_err(|_| ControlError::Invalid("Invalid recording duration"))?
            .unwrap_or(0);
        if seconds > i32::MAX as u32 {
            return Err(ControlError::Invalid("Invalid recording duration"));
        }
        if seconds != 0 {
            return Err(ControlError::Unsupported);
        }
    }
    let policy = RecordingIfExists::try_from(
        parameters
            .get("ifExists")
            .map(String::as_str)
            .unwrap_or("fail"),
    )
    .map_err(|_| ControlError::Invalid("Invalid recording conflict policy"))?;
    if policy == RecordingIfExists::Append {
        return Err(ControlError::Unsupported);
    }
    match parameters
        .get("beep")
        .map(String::as_str)
        .unwrap_or("false")
    {
        "false" => {}
        "true" => return Err(ControlError::Unsupported),
        _ => return Err(ControlError::Invalid("Invalid beep parameter")),
    }
    let termination = parameters
        .get("terminateOn")
        .map(String::as_str)
        .unwrap_or("none");
    if !matches!(termination, "" | "none") {
        if termination == "any"
            || (termination.len() == 1
                && termination
                    .chars()
                    .next()
                    .and_then(|symbol| DtmfDigit::from_symbol(symbol.to_ascii_uppercase()))
                    .is_some())
        {
            return Err(ControlError::Unsupported);
        }
        return Err(ControlError::Invalid("Invalid recording termination"));
    }
    Ok(Arguments { name, policy })
}
