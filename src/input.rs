use libafl::inputs::{HasTargetBytes, Input};
use libafl_bolts::ownedref::OwnedSlice;
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
        && text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'-' | b'+' | b'.' | b'e' | b'E'))
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

impl HasTargetBytes for HttpInput {
    fn target_bytes(&self) -> OwnedSlice<'_, u8> {
        OwnedSlice::from(self.nyx_payload())
    }
}

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
            redqueen: false,
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

    fn nyx_payload(&self) -> Vec<u8> {
        let body = self.body_bytes();
        let body_text = String::from_utf8_lossy(&body).into_owned();
        let mut request = serde_json::Map::new();
        let mut uri = self.path.clone();
        let query = self.query_string();
        if !query.is_empty() {
            uri.push('?');
            uri.push_str(&query);
        }

        for (key, value) in [
            ("SCRIPT_FILENAME", "/var/www/html/index.php".to_string()),
            ("SCRIPT_NAME", "/index.php".to_string()),
            ("REQUEST_METHOD", self.method.clone()),
            ("REQUEST_URI", uri),
            ("QUERY_STRING", query),
            ("CONTENT_TYPE", self.content_type().to_string()),
            ("CONTENT_LENGTH", body.len().to_string()),
            ("POST_DATA", body_text),
            ("HTTP_COOKIE", self.cookie_header()),
            ("SERVER_PROTOCOL", "HTTP/1.1".to_string()),
            ("SERVER_NAME", "localhost".to_string()),
            ("SERVER_ADDR", "127.0.0.1".to_string()),
            ("SERVER_PORT", "8000".to_string()),
            ("HTTP_HOST", "localhost:8000".to_string()),
            ("REDIRECT_STATUS", "1".to_string()),
        ] {
            request.insert(key.to_string(), serde_json::Value::String(value));
        }
        for (name, value) in &self.headers {
            let name = name.trim().to_ascii_uppercase().replace('-', "_");
            if !name.is_empty() {
                request.insert(
                    format!("HTTP_{name}"),
                    serde_json::Value::String(String::from_utf8_lossy(value).into_owned()),
                );
            }
        }

        let mut config = serde_json::Map::new();
        let nyx_cpu = std::env::var("ATROPOS_NYX_CPU")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(0)
            .to_string();
        if self.coverage_dump {
            config.insert(
                "COVERAGE_DUMP".to_string(),
                serde_json::Value::String(nyx_cpu),
            );
        }
        if self.exec_limit > 0 {
            config.insert(
                "EXEC_LIMIT".to_string(),
                serde_json::Value::String(self.exec_limit.to_string()),
            );
        }
        let payload = serde_json::json!({
            "config": config,
            "requests": [request],
        });
        let mut bytes = serde_json::to_vec(&payload).expect("Nyx payload serialization");
        // The guest agent reads this buffer as a C string before parsing its JSON.
        bytes.push(0);
        bytes
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
