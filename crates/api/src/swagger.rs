use serde_json::{json, Map, Value};
use std::collections::VecDeque;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SwaggerError {
    Schema(String),
    Reference(String),
    Conflict(String),
    Capacity,
}

impl std::fmt::Display for SwaggerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Schema(path) => write!(formatter, "Unsupported Swagger schema at {path}"),
            Self::Reference(name) => write!(formatter, "Unknown Swagger reference {name}"),
            Self::Conflict(name) => write!(formatter, "Conflicting Swagger model {name}"),
            Self::Capacity => formatter.write_str("Swagger schema capacity exceeded"),
        }
    }
}

impl std::error::Error for SwaggerError {}

struct Models {
    definitions: Map<String, Value>,
    pending: VecDeque<(String, Value)>,
    output: Map<String, Value>,
}

impl Models {
    fn enqueue(&mut self, name: String, schema: Value) -> Result<(), SwaggerError> {
        if self.output.contains_key(&name) || self.pending.iter().any(|(other, _)| other == &name) {
            return Err(SwaggerError::Conflict(name));
        }
        if self.output.len() + self.pending.len() >= 128 {
            return Err(SwaggerError::Capacity);
        }
        self.pending.push_back((name, schema));
        Ok(())
    }

    fn property(
        &mut self,
        schema: &Value,
        path: &str,
        depth: usize,
        optional: bool,
    ) -> Result<Value, SwaggerError> {
        if depth > 32 {
            return Err(SwaggerError::Capacity);
        }
        if let Some(branches) = schema.get("anyOf").and_then(Value::as_array) {
            let is_null =
                |branch: &&Value| branch.get("type").and_then(Value::as_str) == Some("null");
            if !optional || branches.len() != 2 || branches.iter().filter(is_null).count() != 1 {
                return Err(SwaggerError::Schema(path.into()));
            }
            let branch = branches
                .iter()
                .find(|branch| !is_null(branch))
                .ok_or_else(|| SwaggerError::Schema(path.into()))?;
            let mut output = self.property(branch, path, depth + 1, false)?;
            if let Some(description) = schema.get("description") {
                output["description"] = description.clone();
            }
            if let Some(default) = schema.get("default").filter(|value| !value.is_null()) {
                output["defaultValue"] = default.clone();
            }
            return Ok(output);
        }
        if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
            let name = reference
                .strip_prefix("#/components/schemas/")
                .ok_or_else(|| SwaggerError::Reference(reference.into()))?;
            let definition = self
                .definitions
                .get(name)
                .ok_or_else(|| SwaggerError::Reference(name.into()))?;
            if definition.get("properties").is_some() || definition.get("oneOf").is_some() {
                return Ok(json!({"$ref": name}));
            }
            let definition = definition.clone();
            return self.property(&definition, path, depth + 1, optional);
        }
        let kind = match schema.get("type") {
            Some(Value::String(kind)) => kind.as_str(),
            Some(Value::Array(kinds))
                if optional && kinds.len() == 2 && kinds.iter().any(|kind| kind == "null") =>
            {
                kinds
                    .iter()
                    .filter_map(Value::as_str)
                    .find(|kind| *kind != "null")
                    .ok_or_else(|| SwaggerError::Schema(path.into()))?
            }
            Some(
                Value::Array(_)
                | Value::Object(_)
                | Value::Number(_)
                | Value::Bool(_)
                | Value::Null,
            )
            | None => return Err(SwaggerError::Schema(path.into())),
        };
        if kind == "object" {
            self.enqueue(path.into(), schema.clone())?;
            return Ok(json!({"$ref": path}));
        }
        if !matches!(kind, "string" | "integer" | "number" | "boolean" | "array") {
            return Err(SwaggerError::Schema(path.into()));
        }
        let mut output = json!({"type": kind});
        if kind == "array" {
            let items = schema
                .get("items")
                .ok_or_else(|| SwaggerError::Schema(path.into()))?;
            if items.get("type").and_then(Value::as_str) == Some("array") {
                return Err(SwaggerError::Schema(path.into()));
            }
            output["items"] = self.property(items, &format!("{path}_item"), depth + 1, false)?;
        }
        if let Some(format) = schema
            .get("format")
            .and_then(Value::as_str)
            .filter(|format| *format != "uint64")
        {
            output["format"] = Value::String(
                match format {
                    "uint8" | "uint16" => "int32",
                    "uint32" => "int64",
                    "int32" | "int64" | "float" | "double" | "byte" | "date" | "date-time" => {
                        format
                    }
                    _ => return Err(SwaggerError::Schema(path.into())),
                }
                .into(),
            );
        }
        for key in ["enum", "description", "uniqueItems"] {
            if let Some(value) = schema.get(key) {
                output[key] = value.clone();
            }
        }
        if let Some(value) = schema.get("const") {
            output["enum"] = crate::json_array([value.clone()]);
        }
        if let Some(value) = schema.get("default").filter(|value| !value.is_null()) {
            output["defaultValue"] = value.clone();
        }
        for key in ["minimum", "maximum"] {
            if let Some(value) = schema.get(key) {
                output[key] = Value::String(value.to_string());
            }
        }
        Ok(output)
    }

    fn object(&mut self, name: &str, schema: &Value) -> Result<Value, SwaggerError> {
        let mut properties = Map::new();
        if let Some(fields) = schema.get("properties").and_then(Value::as_object) {
            for (field, value) in fields {
                let optional = !schema
                    .get("required")
                    .and_then(Value::as_array)
                    .is_some_and(|required| required.iter().any(|name| name == field));
                properties.insert(
                    field.clone(),
                    self.property(value, &format!("{name}_{field}"), 0, optional)?,
                );
            }
        }
        let mut output = json!({"id": name, "properties": properties});
        if let Some(required) = schema.get("required") {
            output["required"] = required.clone();
        }
        Ok(output)
    }

    fn event(
        &mut self,
        name: &str,
        schema: &Value,
        branches: &[Value],
    ) -> Result<Value, SwaggerError> {
        let mut base = schema.clone();
        base.as_object_mut()
            .ok_or_else(|| SwaggerError::Schema(name.into()))?
            .remove("oneOf");
        let mut subtypes = Vec::with_capacity(branches.len());
        let mut required = base
            .get("required")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        required.push(Value::String("type".into()));
        base["required"] = Value::Array(required);
        base["properties"]["type"] = json!({"type": "string"});
        for branch in branches {
            let subtype = branch
                .pointer("/properties/type/const")
                .and_then(Value::as_str)
                .ok_or_else(|| SwaggerError::Schema(name.into()))?;
            let mut definition = branch.clone();
            definition["properties"]
                .as_object_mut()
                .ok_or_else(|| SwaggerError::Schema(subtype.into()))?
                .remove("type");
            if let Some(required) = definition.get_mut("required").and_then(Value::as_array_mut) {
                required.retain(|value| value != "type");
            }
            self.enqueue(subtype.into(), definition)?;
            subtypes.push(Value::String(subtype.into()));
        }
        let mut output = self.object(name, &base)?;
        output["subTypes"] = Value::Array(subtypes);
        output["discriminator"] = Value::String("type".into());
        Ok(output)
    }
}

pub(crate) fn models() -> Result<Value, SwaggerError> {
    let definitions = crate::schemas()
        .as_object()
        .cloned()
        .ok_or_else(|| SwaggerError::Schema("models".into()))?;
    let pending = definitions
        .iter()
        .filter(|(_, schema)| schema.get("type").and_then(Value::as_str) == Some("object"))
        .map(|(name, schema)| (name.clone(), schema.clone()))
        .collect();
    let mut models = Models {
        definitions,
        pending,
        output: Map::new(),
    };
    while let Some((name, schema)) = models.pending.pop_front() {
        let model = if let Some(branches) = schema.get("oneOf").and_then(Value::as_array) {
            models.event(&name, &schema, branches)?
        } else {
            models.object(&name, &schema)?
        };
        if models.output.insert(name.clone(), model).is_some() {
            return Err(SwaggerError::Conflict(name));
        }
    }
    Ok(Value::Object(models.output))
}

#[cfg(test)]
mod tests {
    use super::{Models, SwaggerError};
    use serde_json::{json, Map};
    use std::collections::VecDeque;

    #[test]
    fn optional_references_preserve_enum_values_and_reject_ambiguous_unions(
    ) -> Result<(), SwaggerError> {
        let mut definitions = Map::new();
        let values = crate::json_array([json!("offline"), json!("online")]);
        definitions.insert(
            "availability".into(),
            json!({"type":"string", "enum":values}),
        );
        let mut models = Models {
            definitions,
            pending: VecDeque::new(),
            output: Map::new(),
        };
        let branches = crate::json_array([
            json!({"$ref":"#/components/schemas/availability"}),
            json!({"type":"null"}),
        ]);
        let schema = json!({"anyOf":branches, "description":"Endpoint availability"});
        let property = models.property(&schema, "state", 0, true)?;
        assert_eq!(property["type"], "string");
        assert_eq!(property["enum"], values);
        assert_eq!(property["description"], "Endpoint availability");
        assert!(matches!(
            models.property(&schema, "state", 0, false),
            Err(SwaggerError::Schema(_))
        ));
        for branches in [
            crate::json_array([json!({"type":"null"}), json!({"type":"null"})]),
            crate::json_array([json!({"type":"string"}), json!({"type":"integer"})]),
            crate::json_array([
                json!({"type":"string"}),
                json!({"type":"integer"}),
                json!({"type":"null"}),
            ]),
        ] {
            assert!(matches!(
                models.property(&json!({"anyOf":branches}), "state", 0, true),
                Err(SwaggerError::Schema(_))
            ));
        }
        Ok(())
    }

    #[test]
    fn optional_primitive_fields_use_omission_and_required_nullable_fields_are_rejected(
    ) -> Result<(), SwaggerError> {
        let mut models = Models {
            definitions: Map::new(),
            pending: VecDeque::new(),
            output: Map::new(),
        };
        let required = serde_json::to_value(["name"])
            .map_err(|error| SwaggerError::Schema(error.to_string()))?;
        let nullable = serde_json::to_value(["string", "null"])
            .map_err(|error| SwaggerError::Schema(error.to_string()))?;
        let mut schema = json!({"type":"object", "properties":{"cause":{"type":nullable}, "name":{"type":"string"}}, "required":required});
        let projected = models.object("recording", &schema)?;
        assert_eq!(projected["properties"]["cause"]["type"], "string");
        assert_eq!(projected["required"], schema["required"]);
        schema["required"] = serde_json::to_value(["name", "cause"])
            .map_err(|error| SwaggerError::Schema(error.to_string()))?;
        assert!(matches!(
            models.object("recording", &schema),
            Err(SwaggerError::Schema(_))
        ));
        Ok(())
    }
}
