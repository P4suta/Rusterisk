use super::{id, required};
use crate::ControlError;
use rusterisk_api::{
    ChannelId, RecordingIfExists, RecordingOptions, RecordingSeconds, RecordingTermination,
};
use std::collections::BTreeMap;

pub(super) struct Arguments {
    pub name: ChannelId,
    pub options: RecordingOptions,
}

fn seconds(
    parameters: &BTreeMap<String, String>,
    name: &str,
) -> Result<RecordingSeconds, ControlError> {
    let seconds = parameters
        .get(name)
        .map(|value| value.parse::<u32>())
        .transpose()
        .map_err(|_| ControlError::Invalid("Invalid recording duration"))?
        .unwrap_or(0);
    RecordingSeconds::try_from(seconds)
        .map_err(|_| ControlError::Invalid("Invalid recording duration"))
}

pub(super) fn arguments(parameters: &BTreeMap<String, String>) -> Result<Arguments, ControlError> {
    let name = id(required(parameters, "name")?)?;
    let format = required(parameters, "format")?;
    if format != "wav" {
        return Err(ControlError::Unprocessable(
            "Specified format is unknown on this system",
        ));
    }
    let max_duration_seconds = seconds(parameters, "maxDurationSeconds")?;
    let max_silence_seconds = seconds(parameters, "maxSilenceSeconds")?;
    let if_exists = RecordingIfExists::try_from(
        parameters
            .get("ifExists")
            .map(String::as_str)
            .unwrap_or("fail"),
    )
    .map_err(|_| ControlError::Invalid("Invalid recording conflict policy"))?;
    let beep = match parameters
        .get("beep")
        .map(String::as_str)
        .unwrap_or("false")
    {
        "false" => false,
        "true" => true,
        _ => return Err(ControlError::Invalid("Invalid beep parameter")),
    };
    let terminate_on = RecordingTermination::try_from(
        parameters
            .get("terminateOn")
            .map(String::as_str)
            .unwrap_or("none"),
    )
    .map_err(|_| ControlError::Invalid("Invalid recording termination"))?;
    Ok(Arguments {
        name,
        options: RecordingOptions {
            if_exists,
            max_duration_seconds,
            max_silence_seconds,
            beep,
            terminate_on,
        },
    })
}
