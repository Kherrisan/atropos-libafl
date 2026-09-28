use libafl::inputs::Input;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, Hash, PartialEq, Eq)]
pub enum JsonValue {
    Null,
    Bool(bool),
    Number(String),
    String(Vec<u8>),
    Array(Vec<JsonValue>),
    Object(Vec<(String, JsonValue)>),
}

impl JsonValue {
    pub fn from_json(value: &serde_json::Value) -> Self {
        match value {
            serde_json::Value::Null => Self::Null,
            serde_json::Value::Bool(bit) => Self::Bool(*bit),
            serde_json::Value::Number(number) => Self::Number(number.to_string()),
            serde_json::Value::String(text) => Self::String(text.as_bytes().to_vec()),
            serde_json::Value::Array(items) => {
                Self::Array(items.iter().map(Self::from_json).collect())
            }
            serde_json::Value::Object(map) => Self::Object(
                map.iter()
                    .map(|(key, value)| (key.clone(), Self::from_json(value)))
                    .collect(),
            ),
        }
    }

    pub fn write_json(&self, out: &mut Vec<u8>) {
        match self {
            Self::Null => out.extend_from_slice(b"null"),
            Self::Bool(true) => out.extend_from_slice(b"true"),
            Self::Bool(false) => out.extend_from_slice(b"false"),
            Self::Number(text) => {
                if is_json_number(text) {
                    out.extend_from_slice(text.as_bytes());
                } else {
                    push_json_string(out, text.as_bytes());
                }
            }
            Self::String(bytes) => push_json_string(out, bytes),
            Self::Array(items) => {
                out.push(b'[');
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        out.push(b',');
                    }
                    item.write_json(out);
                }
                out.push(b']');
            }
            Self::Object(fields) => {
                out.push(b'{');
                for (index, (key, value)) in fields.iter().enumerate() {
                    if index > 0 {
                        out.push(b',');
                    }
                    push_json_string(out, key.as_bytes());
                    out.push(b':');
                    value.write_json(out);
                }
                out.push(b'}');
            }
        }
    }

    pub fn string_paths(&self) -> Vec<Vec<usize>> {
        let mut out = Vec::new();
        self.collect_strings(&mut Vec::new(), &mut out);
        out
    }

    fn collect_strings(&self, prefix: &mut Vec<usize>, out: &mut Vec<Vec<usize>>) {
        match self {
            Self::String(_) | Self::Number(_) => out.push(prefix.clone()),
            Self::Array(items) => {
                for (index, item) in items.iter().enumerate() {
                    prefix.push(index);
                    item.collect_strings(prefix, out);
                    prefix.pop();
                }
            }
            Self::Object(fields) => {
                for (index, (_, value)) in fields.iter().enumerate() {
                    prefix.push(index);
                    value.collect_strings(prefix, out);
                    prefix.pop();
                }
            }
            _ => {}
        }
    }

    pub fn object_paths(&self) -> Vec<Vec<usize>> {
        let mut out = Vec::new();
        self.collect_objects(&mut Vec::new(), &mut out);
        out
    }

    fn collect_objects(&self, prefix: &mut Vec<usize>, out: &mut Vec<Vec<usize>>) {
        match self {
            Self::Array(items) => {
                for (index, item) in items.iter().enumerate() {
                    prefix.push(index);
                    item.collect_objects(prefix, out);
                    prefix.pop();
                }
            }
            Self::Object(fields) => {
                out.push(prefix.clone());
                for (index, (_, value)) in fields.iter().enumerate() {
                    prefix.push(index);
                    value.collect_objects(prefix, out);
                    prefix.pop();
                }
            }
            _ => {}
        }
    }

    pub fn leaf_ref(&self, path: &[usize]) -> Option<&JsonValue> {
        let mut cursor = self;
        for index in path {
            cursor = match cursor {
                Self::Array(items) => items.get(*index)?,
                Self::Object(fields) => &fields.get(*index)?.1,
                _ => return None,
            };
        }
        Some(cursor)
    }

    pub fn leaf_mut(&mut self, path: &[usize]) -> Option<&mut JsonValue> {
        let mut cursor = self;
        for index in path {
            cursor = match cursor {
                Self::Array(items) => items.get_mut(*index)?,
                Self::Object(fields) => &mut fields.get_mut(*index)?.1,
                _ => return None,
            };
        }
        Some(cursor)
    }

    pub fn object_fields_mut(&mut self, path: &[usize]) -> Option<&mut Vec<(String, JsonValue)>> {
        match self.leaf_mut(path)? {
            Self::Object(fields) => Some(fields),
            _ => None,
        }
    }
}

fn is_json_number(text: &str) -> bool {
    !text.is_empty()
        && text.bytes().all(|byte| byte.is_ascii_digit() || matches!(byte, b'-' | b'+' | b'.' | b'e' | b'E'))
        && text.parse::<f64>().is_ok()
}

fn push_json_string(out: &mut Vec<u8>, bytes: &[u8]) {
    out.push(b'"');
    for &byte in bytes {
        match byte {
            b'"' => out.extend_from_slice(br#"\""#),
            b'\\' => out.extend_from_slice(br#"\\"#),
            b'\n' => out.extend_from_slice(br#"\n"#),
            b'\r' => out.extend_from_slice(br#"\r"#),
            b'\t' => out.extend_from_slice(br#"\t"#),
            0x20..=0x7e => out.push(byte),
            _ => out.extend_from_slice(format!("\\u{byte:04x}").as_bytes()),
        }
    }
    out.push(b'"');
}

#[derive(Clone, Debug, Serialize, Deserialize, Hash, PartialEq, Eq)]
pub struct HttpInput {
    pub method: String,
    pub path: String,
    pub query: Vec<(String, Vec<u8>)>,
    pub headers: Vec<(String, Vec<u8>)>,
    pub cookies: Vec<(String, Vec<u8>)>,
    pub body: JsonValue,
    pub body_override: Option<Vec<u8>>,
    pub operation_key: Option<String>,
    pub pin_route: bool,
    pub exec_limit: u32,
    pub redqueen: bool,
    #[serde(default)]
    pub coverage_dump: bool,
}

impl Input for HttpInput {}

impl HttpInput {
    pub fn batch_seed() -> Self {
        let body = serde_json::json!({
            "validation": "normal",
            "requests": [{
                "method": "POST",
                "path": "/wp/v2/posts",
                "body": {"title": "seed"},
                "headers": {}
            }]
        });
        Self {
            method: "POST".to_string(),
            path: "/wp-json/batch/v1".to_string(),
            query: Vec::new(),
            headers: Vec::new(),
            cookies: Vec::new(),
            body: JsonValue::from_json(&body),
            body_override: None,
            operation_key: Some("POST /wp-json/batch/v1".to_string()),
            pin_route: true,
            exec_limit: 0,
            redqueen: true,
            coverage_dump: false,
        }
    }

    pub fn summary(&self) -> String {
        let owned = String::from_utf8_lossy(&self.body_bytes()).into_owned();
        let body: String = owned.chars().take(180).collect();
        format!("{} {} {body}", self.method, self.path)
    }

    pub fn content_type(&self) -> &'static str {
        "application/json"
    }

    pub fn body_bytes(&self) -> Vec<u8> {
        if let Some(raw) = &self.body_override {
            return raw.clone();
        }
        let mut out = Vec::new();
        self.body.write_json(&mut out);
        out
    }

    pub fn query_string(&self) -> String {
        self.query
            .iter()
            .map(|(key, value)| format!("{key}={}", lossy(value)))
            .collect::<Vec<_>>()
            .join("&")
    }

    pub fn cookie_header(&self) -> String {
        self.cookies
            .iter()
            .map(|(key, value)| format!("{key}={}", lossy(value)))
            .collect::<Vec<_>>()
            .join("; ")
    }
}

fn lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).replace('\0', "")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batch_seed_contains_requests() {
        let seed = HttpInput::batch_seed();
        let body = String::from_utf8(seed.body_bytes()).unwrap();
        assert!(body.contains("\"requests\""));
        assert!(body.contains("seed"));
    }
}
