use crate::ControlError;
use rusterisk_api::{Operation, ParameterKind};
use serde_json::Value;
use std::{borrow::Cow, collections::BTreeMap};

pub(super) struct Fields<'a> {
    pub parameters: Cow<'a, BTreeMap<String, String>>,
    pub variables: BTreeMap<String, String>,
}

fn text(value: &str) -> Result<String, ControlError> {
    if value.len() > 4096 {
        return Err(ControlError::Invalid("JSON parameter value too long"));
    }
    Ok(value.to_owned())
}

fn list(value: &Value) -> Result<Option<String>, ControlError> {
    match value {
        Value::String(value) => {
            if value.contains(',') {
                return Err(ControlError::Invalid("Invalid list parameter value"));
            }
            text(value).map(Some)
        }
        Value::Array(values) => {
            if values.len() > 32 {
                return Err(ControlError::Invalid("Too many parameter values"));
            }
            let mut output = String::new();
            for (index, value) in values.iter().enumerate() {
                let value = value
                    .as_str()
                    .ok_or(ControlError::Invalid("Expected string parameter values"))?;
                if value.len() > 4096 {
                    return Err(ControlError::Invalid("JSON parameter value too long"));
                }
                if value.contains(',') {
                    return Err(ControlError::Invalid("Invalid list parameter value"));
                }
                if index > 0 {
                    output.push(',');
                }
                output.push_str(value);
            }
            Ok(Some(output))
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::Object(_) => Ok(None),
    }
}

pub(super) fn decode<'a>(
    query: &'a BTreeMap<String, String>,
    operation: Option<&Operation>,
    body: &[u8],
) -> Result<Fields<'a>, ControlError> {
    let mut fields = Fields {
        parameters: Cow::Borrowed(query),
        variables: BTreeMap::new(),
    };
    if body.is_empty() {
        return Ok(fields);
    }
    if body.len() > 16384 {
        return Err(ControlError::Invalid("JSON body too long"));
    }
    let value: Value =
        serde_json::from_slice(body).map_err(|_| ControlError::Invalid("Invalid JSON body"))?;
    let object = value
        .as_object()
        .ok_or(ControlError::Invalid("Expected a JSON object"))?;
    if object.len() > 32 {
        return Err(ControlError::Invalid("Too many JSON parameters"));
    }
    let Some(operation) = operation else {
        return Ok(fields);
    };
    for (name, value) in object {
        let Some(kind) = operation.parameter_kind(name) else {
            continue;
        };
        let decoded = match kind {
            ParameterKind::String => value.as_str().map(text).transpose()?,
            ParameterKind::Integer => Some(value.as_i64().unwrap_or(0).to_string()),
            ParameterKind::Boolean => Some(value.as_bool().unwrap_or(false).to_string()),
            ParameterKind::StringList => list(value)?,
            ParameterKind::Variables => {
                let variables = value.as_object().ok_or(ControlError::Invalid(
                    "Only string values in the 'variables' object allowed",
                ))?;
                if variables.len() > 128 {
                    return Err(ControlError::Invalid("Too many channel variables"));
                }
                for (name, value) in variables {
                    if name.is_empty() || name.len() > 128 {
                        return Err(ControlError::Invalid("Invalid variable name"));
                    }
                    let value = value.as_str().ok_or(ControlError::Invalid(
                        "Only string values in the 'variables' object allowed",
                    ))?;
                    fields.variables.insert(name.clone(), text(value)?);
                }
                continue;
            }
            ParameterKind::Filter => {
                return Err(ControlError::Invalid("Invalid format definition"))
            }
        };
        match decoded {
            Some(value) => {
                fields.parameters.to_mut().insert(name.clone(), value);
            }
            None => {
                fields.parameters.to_mut().remove(name);
            }
        }
    }
    if fields.parameters.len() > 32 {
        return Err(ControlError::Invalid("Too many query parameters"));
    }
    Ok(fields)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn operation(id: &str) -> Result<&'static Operation, ControlError> {
        rusterisk_api::OPERATIONS
            .iter()
            .find(|operation| operation.id == id)
            .ok_or(ControlError::Unsupported)
    }

    #[test]
    fn typed_body_values_preserve_reference_scalar_conversions_and_list_boundaries(
    ) -> Result<(), ControlError> {
        let query = BTreeMap::from([
            ("name".into(), "query".into()),
            ("beep".into(), "true".into()),
        ]);
        let fields = decode(
            &query,
            Some(operation("channels.record")?),
            br#"{"name":null,"beep":"true","maxDurationSeconds":12}"#,
        )?;
        assert!(!fields.parameters.contains_key("name"));
        assert_eq!(
            fields.parameters.get("beep").map(String::as_str),
            Some("false")
        );
        assert_eq!(
            fields
                .parameters
                .get("maxDurationSeconds")
                .map(String::as_str),
            Some("12")
        );
        let fields = decode(
            &query,
            Some(operation("channels.record")?),
            br#"{"beep":true,"maxDurationSeconds":"12"}"#,
        )?;
        assert_eq!(
            fields.parameters.get("beep").map(String::as_str),
            Some("true")
        );
        assert_eq!(
            fields
                .parameters
                .get("maxDurationSeconds")
                .map(String::as_str),
            Some("0")
        );
        let fields = decode(
            &query,
            Some(operation("bridges.addChannel")?),
            br#"{"channel":["first","second"]}"#,
        )?;
        assert_eq!(
            fields.parameters.get("channel").map(String::as_str),
            Some("first,second")
        );
        for body in [
            br#"{"channel":["first,second"]}"#.as_slice(),
            br#"{"channel":["first",42]}"#,
        ] {
            assert!(decode(&query, Some(operation("bridges.addChannel")?), body).is_err());
        }
        Ok(())
    }

    #[test]
    fn body_limits_reject_excess_fields_values_and_variable_types_before_admission(
    ) -> Result<(), ControlError> {
        let query = BTreeMap::new();
        assert!(decode(&query, None, &[b' '; 16385]).is_err());
        let fields: BTreeMap<String, i32> = (0..33)
            .map(|index| (format!("field{index}"), index))
            .collect();
        assert!(decode(&query, None, &serde_json::to_vec(&fields)?).is_err());
        let values = serde_json::json!({"channel": vec!["id"; 33]});
        assert!(decode(
            &query,
            Some(operation("bridges.addChannel")?),
            &serde_json::to_vec(&values)?
        )
        .is_err());
        let name = serde_json::json!({"name": "x".repeat(4097)});
        assert!(decode(
            &query,
            Some(operation("bridges.create")?),
            &serde_json::to_vec(&name)?
        )
        .is_err());
        for variables in [r#"{"X":42}"#, r#"["X"]"#, r#"{"":"empty name"}"#] {
            let variables: Value = serde_json::from_str(variables)?;
            let body = serde_json::to_vec(&serde_json::json!({"variables": variables}))?;
            assert!(decode(&query, Some(operation("channels.originate")?), &body).is_err());
        }
        let body = br#"{"variables":{"FIRST":"one","SECOND":"two"},"endpoint":"PJSIP/phone"}"#;
        let fields = decode(&query, Some(operation("channels.originate")?), body)?;
        assert_eq!(fields.variables.len(), 2);
        assert_eq!(
            fields.variables.get("SECOND").map(String::as_str),
            Some("two")
        );
        assert!(!fields.parameters.contains_key("variables"));
        Ok(())
    }
}
