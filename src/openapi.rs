use serde_yaml::Value;

use crate::input::{HttpInput, JsonValue};

#[derive(Clone, Debug)]
pub enum Schema {
    Any,
    Enum(Vec<String>),
    Object(Vec<(String, Schema)>),
    Array(Box<Schema>),
    String,
    Number,
    Bool,
}

#[derive(Clone, Debug)]
pub struct Operation {
    pub method: String,
    pub path_template: String,
    pub body: Option<Schema>,
    pub key: String,
}

impl Operation {
    pub fn to_input(&self) -> HttpInput {
        let body = self
            .body
            .as_ref()
            .map(example_json)
            .unwrap_or(JsonValue::Object(Vec::new()));
        HttpInput {
            method: self.method.clone(),
            path: self.path_template.clone(),
            query: Vec::new(),
            headers: Vec::new(),
            cookies: Vec::new(),
            body,
            body_override: None,
            operation_key: Some(self.key.clone()),
            pin_route: true,
            exec_limit: 0,
            redqueen: false,
            coverage_dump: false,
        }
    }
}

pub fn load_operations(path: &str) -> Result<Vec<Operation>, String> {
    let text = std::fs::read_to_string(path).map_err(|err| err.to_string())?;
    let spec: Value = serde_yaml::from_str(&text).map_err(|err| err.to_string())?;
    let mut operations = Vec::new();
    let paths = spec.get("paths").and_then(Value::as_mapping);
    let Some(paths) = paths else {
        return Ok(operations);
    };
    for (template, item) in paths {
        let Some(template) = template.as_str() else {
            continue;
        };
        let Some(item) = item.as_mapping() else {
            continue;
        };
        for method in ["get", "post", "put", "patch", "delete", "head", "options"] {
            let Some(operation) = item.get(Value::String(method.to_string())) else {
                continue;
            };
            let body = json_schema(operation, &spec).map(|schema| schema_from(&schema, &spec));
            let method = method.to_uppercase();
            let key = format!("{method} {template}");
            operations.push(Operation {
                method,
                path_template: template.to_string(),
                body,
                key,
            });
        }
    }
    Ok(operations)
}

fn json_schema<'a>(operation: &'a Value, root: &'a Value) -> Option<Value> {
    let body = resolve(operation.get("requestBody")?, root);
    let media = body.get("content")?.get("application/json")?;
    Some(resolve(media.get("schema")?, root))
}

fn resolve(schema: &Value, root: &Value) -> Value {
    resolve_seen(schema, root, &mut Vec::new())
}

fn resolve_seen(schema: &Value, root: &Value, seen: &mut Vec<String>) -> Value {
    if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
        if seen.iter().any(|item| item == reference) {
            return Value::Null;
        }
        seen.push(reference.to_string());
        let mut node = root;
        if let Some(path) = reference.strip_prefix("#/") {
            for part in path.split('/') {
                let part = part.replace("~1", "/").replace("~0", "~");
                node = match node.get(&part) {
                    Some(next) => next,
                    None => return Value::Null,
                };
            }
        }
        return resolve_seen(node, root, seen);
    }
    if let Some(parts) = schema.get("allOf").and_then(Value::as_sequence) {
        let mut merged = serde_yaml::Mapping::new();
        let mut properties = serde_yaml::Mapping::new();
        let mut required = Vec::new();
        for part in parts {
            let piece = resolve_seen(part, root, seen);
            if let Some(map) = piece.as_mapping() {
                if let Some(props) = map.get("properties").and_then(Value::as_mapping) {
                    for (key, value) in props {
                        properties.insert(key.clone(), value.clone());
                    }
                }
                if let Some(items) = map.get("required").and_then(Value::as_sequence) {
                    required.extend(items.iter().cloned());
                }
                for (key, value) in map {
                    if key.as_str() != Some("properties") && key.as_str() != Some("required") {
                        merged.insert(key.clone(), value.clone());
                    }
                }
            }
        }
        merged.insert(
            Value::String("properties".into()),
            Value::Mapping(properties),
        );
        merged.insert(Value::String("required".into()), Value::Sequence(required));
        return Value::Mapping(merged);
    }
    schema.clone()
}

fn schema_from(schema: &Value, root: &Value) -> Schema {
    let schema = resolve(schema, root);
    if let Some(items) = schema.get("enum").and_then(Value::as_sequence) {
        return Schema::Enum(
            items
                .iter()
                .filter_map(|item| {
                    item.as_str()
                        .map(str::to_string)
                        .or_else(|| item.as_i64().map(|number| number.to_string()))
                })
                .collect(),
        );
    }
    match schema.get("type").and_then(Value::as_str) {
        Some("object") | None if schema.get("properties").is_some() => {
            let props = schema
                .get("properties")
                .and_then(Value::as_mapping)
                .map(|map| {
                    map.iter()
                        .filter_map(|(key, value)| {
                            key.as_str()
                                .map(|name| (name.to_string(), schema_from(value, root)))
                        })
                        .collect()
                })
                .unwrap_or_default();
            Schema::Object(props)
        }
        Some("array") => {
            let item = schema
                .get("items")
                .cloned()
                .unwrap_or(Value::String("string".into()));
            Schema::Array(Box::new(schema_from(&item, root)))
        }
        Some("boolean") => Schema::Bool,
        Some("integer") | Some("number") => Schema::Number,
        Some("string") => Schema::String,
        _ => Schema::Any,
    }
}

fn example_json(schema: &Schema) -> JsonValue {
    match schema {
        Schema::Enum(items) => {
            JsonValue::String(items.first().cloned().unwrap_or_default().into_bytes())
        }
        Schema::Object(fields) => JsonValue::Object(
            fields
                .iter()
                .map(|(name, child)| (name.clone(), example_json(child)))
                .collect(),
        ),
        Schema::Array(item) => JsonValue::Array(vec![example_json(item)]),
        Schema::Bool => JsonValue::Bool(false),
        Schema::Number => JsonValue::Number("0".to_string()),
        Schema::String | Schema::Any => JsonValue::String(Vec::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_batch_operation() {
        let ops = load_operations("/home/user/WuppieFuzz/wordpress/openapi.yaml").unwrap();
        assert_eq!(ops.len(), 1);
        let input = ops[0].to_input();
        let body = String::from_utf8(input.body_bytes()).unwrap();
        assert!(body.contains("requests"));
        assert!(body.contains("title"));
        assert!(body.contains("method"));
        assert_eq!(input.method, "POST");
        assert_eq!(input.path, "/wp-json/batch/v1");
    }
}
