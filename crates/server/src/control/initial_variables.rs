use super::{validate_variable, CallerField, ControlError};
use rusterisk_api::Caller;
use std::collections::BTreeMap;

pub(super) struct InitialVariables {
    pub values: BTreeMap<String, String>,
    pub caller: Caller,
}

pub(super) fn caller_value(
    mut caller: Caller,
    field: CallerField,
    value: &str,
) -> Result<Caller, ControlError> {
    if value.len() > 4096 || value.chars().any(char::is_control) {
        return Err(ControlError::Invalid("Invalid caller identification"));
    }
    match field {
        CallerField::Name => caller.name = value.into(),
        CallerField::Number => caller.number = value.into(),
        CallerField::All => {
            caller = if let Some((name, number)) = value.split_once('<') {
                let number = number
                    .trim()
                    .strip_suffix('>')
                    .ok_or(ControlError::Invalid("Invalid caller identification"))?;
                Caller {
                    name: name.trim().trim_matches('"').into(),
                    number: number.trim().into(),
                }
            } else {
                Caller {
                    name: String::new(),
                    number: value.trim().into(),
                }
            };
        }
    }
    if caller.name.len() > 128 || caller.number.len() > 128 {
        return Err(ControlError::Invalid(
            "Caller identification exceeds 128 bytes",
        ));
    }
    Ok(caller)
}

impl InitialVariables {
    pub fn new(values: BTreeMap<String, String>, mut caller: Caller) -> Result<Self, ControlError> {
        if values.len() > 64 {
            return Err(ControlError::Capacity);
        }
        let mut ordinary = BTreeMap::new();
        for (name, value) in values {
            let field = match name.as_str() {
                "CALLERID(name)" => Some(CallerField::Name),
                "CALLERID(num)" => Some(CallerField::Number),
                "CALLERID(all)" => Some(CallerField::All),
                _ => None,
            };
            if let Some(field) = field {
                caller = caller_value(caller, field, &value)?;
            } else {
                validate_variable(&name)?;
                if matches!(
                    name.as_str(),
                    "EXTEN" | "CONTEXT" | "PRIORITY" | "RUSTERISK_MEDIA_CONNECTED"
                ) {
                    return Err(ControlError::Unsupported);
                }
                if value.len() > 4096 || value.chars().any(char::is_control) {
                    return Err(ControlError::Invalid("Invalid variable value"));
                }
                ordinary.insert(name, value);
            }
        }
        Ok(Self {
            values: ordinary,
            caller,
        })
    }
}
