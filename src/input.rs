use std::path::Path;

use libafl::{
    inputs::{HasTargetBytes, Input},
    Error,
};
use libafl_bolts::{fs::write_file_atomic, ownedref::OwnedSlice};
use serde::{Deserialize, Serialize};

/// Keep the Nyx input buffer and structured-mutator limit in sync.
pub const NYX_INPUT_BUFFER_SIZE: usize = 1024 * 1024;

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

    /// Returns paths to this value and all of its descendants.
    pub fn value_paths(&self) -> Vec<Vec<usize>> {
        let mut out = Vec::new();
        self.collect_values(&mut Vec::new(), &mut out);
        out
    }

    fn collect_values(&self, prefix: &mut Vec<usize>, out: &mut Vec<Vec<usize>>) {
        out.push(prefix.clone());
        match self {
            Self::Array(items) => {
                for (index, item) in items.iter().enumerate() {
                    prefix.push(index);
                    item.collect_values(prefix, out);
                    prefix.pop();
                }
            }
            Self::Object(fields) => {
                for (index, (_, value)) in fields.iter().enumerate() {
                    prefix.push(index);
                    value.collect_values(prefix, out);
                    prefix.pop();
                }
            }
            Self::Null | Self::Bool(_) | Self::Number(_) | Self::String(_) => {}
        }
    }

    /// Returns paths to all arrays in this value tree.
    pub fn array_paths(&self) -> Vec<Vec<usize>> {
        let mut out = Vec::new();
        self.collect_arrays(&mut Vec::new(), &mut out);
        out
    }

    fn collect_arrays(&self, prefix: &mut Vec<usize>, out: &mut Vec<Vec<usize>>) {
        match self {
            Self::Array(items) => {
                out.push(prefix.clone());
                for (index, item) in items.iter().enumerate() {
                    prefix.push(index);
                    item.collect_arrays(prefix, out);
                    prefix.pop();
                }
            }
            Self::Object(fields) => {
                for (index, (_, value)) in fields.iter().enumerate() {
                    prefix.push(index);
                    value.collect_arrays(prefix, out);
                    prefix.pop();
                }
            }
            Self::Null | Self::Bool(_) | Self::Number(_) | Self::String(_) => {}
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

    pub fn array_items_mut(&mut self, path: &[usize]) -> Option<&mut Vec<JsonValue>> {
        match self.leaf_mut(path)? {
            Self::Array(items) => Some(items),
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

impl Input for HttpInput {
    fn to_file<P>(&self, path: P) -> Result<(), Error>
    where
        P: AsRef<Path>,
    {
        let path = path.as_ref();
        let bytes = postcard::to_allocvec(self)
            .map_err(|err| Error::serialize(format!("HttpInput postcard: {err}")))?;
        write_file_atomic(path, &bytes)?;

        // Viewing copy only. The fuzzer reloads the postcard file next to it.
        let mut json = serde_json::to_vec_pretty(&self.plaintext_json())
            .map_err(|err| Error::serialize(format!("HttpInput plaintext: {err}")))?;
        json.push(b'\n');
        write_file_atomic(path.with_extension("json"), &json)
    }
}

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

    /// Human-readable request. This is not a second corpus format.
    pub(crate) fn plaintext_json(&self) -> serde_json::Value {
        serde_json::json!({
            "method": self.method,
            "path": self.path,
            "query": pairs_plain(&self.query),
            "headers": pairs_plain(&self.headers),
            "cookies": pairs_plain(&self.cookies),
            "body": json_value_plain(&self.body),
            "body_override": self.body_override.as_deref().map(override_plain),
            "operation_key": self.operation_key,
            "pin_route": self.pin_route,
            "exec_limit": self.exec_limit,
            "redqueen": self.redqueen,
            "coverage_dump": self.coverage_dump,
        })
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

fn json_value_plain(value: &JsonValue) -> serde_json::Value {
    let mut bytes = Vec::new();
    value.write_json(&mut bytes);
    serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| serde_json::Value::String(String::from_utf8_lossy(&bytes).into_owned()))
}

fn override_plain(bytes: &[u8]) -> serde_json::Value {
    let text = lossy(bytes);
    serde_json::from_str(&text).unwrap_or_else(|_| serde_json::Value::String(text))
}

fn pairs_plain(pairs: &[(String, Vec<u8>)]) -> serde_json::Value {
    let mut seen = std::collections::BTreeSet::new();
    let unique = pairs.iter().all(|(key, _)| seen.insert(key.as_str()));
    if unique {
        let mut map = serde_json::Map::new();
        for (key, value) in pairs {
            map.insert(key.clone(), serde_json::Value::String(lossy(value)));
        }
        return serde_json::Value::Object(map);
    }
    serde_json::Value::Array(
        pairs
            .iter()
            .map(|(key, value)| serde_json::json!([key, lossy(value)]))
            .collect(),
    )
}

const SEED_FIELDS: &[&str] = &[
    "method",
    "path",
    "query",
    "headers",
    "cookies",
    "body",
    "body_override",
    "operation_key",
    "pin_route",
    "exec_limit",
    "redqueen",
    "coverage_dump",
];

/// Load one seed from each top-level `*.json` file that matches the seed format.
///
/// A seed object requires `method`, `path`, and `body`. `query`, `headers`, and
/// `cookies` are string maps or arrays of `[key, value]` pairs. `pin_route`
/// defaults to true. Files that do not match are skipped.
pub fn load_seed_directory(dir: &std::path::Path) -> Result<Vec<HttpInput>, String> {
    if !dir.is_dir() {
        return Err(format!("seed directory does not exist: {}", dir.display()));
    }

    let mut names = Vec::new();
    for entry in std::fs::read_dir(dir)
        .map_err(|err| format!("cannot read seed directory {}: {err}", dir.display()))?
    {
        let entry =
            entry.map_err(|err| format!("cannot read seed directory {}: {err}", dir.display()))?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if name.starts_with('.') || !name.ends_with(".json") {
            continue;
        }
        if !entry.metadata().is_ok_and(|meta| meta.is_file()) {
            continue;
        }
        names.push(name.to_string());
    }
    names.sort();
    if names.is_empty() {
        return Err(format!(
            "seed directory {} contains no .json files",
            dir.display()
        ));
    }

    let mut seeds = Vec::new();
    let mut skipped = Vec::new();
    for name in names {
        let path = dir.join(&name);
        let text = std::fs::read_to_string(&path)
            .map_err(|err| format!("cannot read seed {}: {err}", path.display()))?;
        match parse_seed_json(&text) {
            Ok(seed) => seeds.push(seed),
            Err(reason) => skipped.push(format!("{name}: {reason}")),
        }
    }
    if seeds.is_empty() {
        return Err(format!(
            "seed directory {} has no JSON file in the seed format: {}",
            dir.display(),
            skipped.join("; ")
        ));
    }
    for reason in &skipped {
        eprintln!("seed: skipped {reason}");
    }
    Ok(seeds)
}

/// Object keys and scalar values from every seed, in seed order, without duplicates.
pub fn dictionary_tokens(seeds: &[HttpInput]) -> Vec<String> {
    let mut tokens = Vec::new();
    for seed in seeds {
        push_token(&mut tokens, &seed.method);
        push_token(&mut tokens, &seed.path);
        for (key, value) in seed.query.iter().chain(&seed.headers).chain(&seed.cookies) {
            push_token(&mut tokens, key);
            push_token(&mut tokens, &String::from_utf8_lossy(value));
        }
        collect_json_tokens(&seed.body, &mut tokens);
        if let Some(key) = &seed.operation_key {
            push_token(&mut tokens, key);
        }
        if let Some(raw) = &seed.body_override {
            let text = String::from_utf8_lossy(raw);
            match serde_json::from_str::<serde_json::Value>(&text) {
                Ok(value) => collect_json_value_tokens(&value, &mut tokens),
                Err(_) => push_token(&mut tokens, &text),
            }
        }
    }
    tokens
}

fn push_token(tokens: &mut Vec<String>, token: &str) {
    if token.is_empty() || tokens.iter().any(|existing| existing == token) {
        return;
    }
    tokens.push(token.to_string());
}

fn collect_json_tokens(value: &JsonValue, tokens: &mut Vec<String>) {
    match value {
        JsonValue::Null => {}
        JsonValue::Bool(bit) => push_token(tokens, if *bit { "true" } else { "false" }),
        JsonValue::Number(text) => push_token(tokens, text),
        JsonValue::String(bytes) => push_token(tokens, &String::from_utf8_lossy(bytes)),
        JsonValue::Array(items) => {
            for item in items {
                collect_json_tokens(item, tokens);
            }
        }
        JsonValue::Object(fields) => {
            for (key, child) in fields {
                push_token(tokens, key);
                collect_json_tokens(child, tokens);
            }
        }
    }
}

fn collect_json_value_tokens(value: &serde_json::Value, tokens: &mut Vec<String>) {
    match value {
        serde_json::Value::Null => {}
        serde_json::Value::Bool(bit) => push_token(tokens, if *bit { "true" } else { "false" }),
        serde_json::Value::Number(number) => push_token(tokens, &number.to_string()),
        serde_json::Value::String(text) => push_token(tokens, text),
        serde_json::Value::Array(items) => {
            for item in items {
                collect_json_value_tokens(item, tokens);
            }
        }
        serde_json::Value::Object(fields) => {
            for (key, child) in fields {
                push_token(tokens, key);
                collect_json_value_tokens(child, tokens);
            }
        }
    }
}

fn parse_seed_json(text: &str) -> Result<HttpInput, String> {
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|err| format!("invalid JSON: {err}"))?;
    let object = value
        .as_object()
        .ok_or_else(|| "seed must be a JSON object".to_string())?;
    for key in object.keys() {
        if !SEED_FIELDS.contains(&key.as_str()) {
            return Err(format!("unknown field {key}"));
        }
    }

    let method = required_string(object, "method")?;
    if method.is_empty() || !method.bytes().all(|byte| byte.is_ascii_alphabetic()) {
        return Err("method must be an HTTP method".to_string());
    }
    let method = method.to_ascii_uppercase();

    let path = required_string(object, "path")?;
    if !path.starts_with('/') || path.contains(['?', '#', ' ', '\0']) {
        return Err(
            "path must start with / and must not contain a query, hash, or space".to_string(),
        );
    }

    let body = object
        .get("body")
        .ok_or_else(|| "missing body".to_string())?;
    let query = pairs_field(object, "query")?;
    let headers = pairs_field(object, "headers")?;
    let cookies = pairs_field(object, "cookies")?;
    let body_override = optional_string(object, "body_override")?.map(|text| text.into_bytes());
    let operation_key = match optional_string(object, "operation_key")? {
        Some(key) if !key.is_empty() => Some(key),
        _ => Some(format!("{method} {path}")),
    };

    let input = HttpInput {
        method,
        path,
        query,
        headers,
        cookies,
        body: JsonValue::from_json(body),
        body_override,
        operation_key,
        pin_route: optional_bool(object, "pin_route")?.unwrap_or(true),
        exec_limit: optional_u32(object, "exec_limit")?.unwrap_or(0),
        redqueen: optional_bool(object, "redqueen")?.unwrap_or(false),
        coverage_dump: optional_bool(object, "coverage_dump")?.unwrap_or(false),
    };
    if input.nyx_payload().len() > NYX_INPUT_BUFFER_SIZE {
        return Err(format!("payload exceeds {NYX_INPUT_BUFFER_SIZE} bytes"));
    }
    Ok(input)
}

fn required_string(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<String, String> {
    match object.get(field) {
        Some(serde_json::Value::String(text)) => Ok(text.trim().to_string()),
        Some(_) => Err(format!("{field} must be a string")),
        None => Err(format!("missing {field}")),
    }
}

fn optional_string(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<Option<String>, String> {
    match object.get(field) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(text)) => Ok(Some(text.trim().to_string())),
        Some(_) => Err(format!("{field} must be a string")),
    }
}

fn optional_bool(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<Option<bool>, String> {
    match object.get(field) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::Bool(bit)) => Ok(Some(*bit)),
        Some(_) => Err(format!("{field} must be a boolean")),
    }
}

fn optional_u32(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<Option<u32>, String> {
    match object.get(field) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::Number(number)) => number
            .as_u64()
            .and_then(|value| u32::try_from(value).ok())
            .map(Some)
            .ok_or_else(|| format!("{field} must be an integer from 0 to 4294967295")),
        Some(_) => Err(format!("{field} must be an integer from 0 to 4294967295")),
    }
}

fn pairs_field(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<Vec<(String, Vec<u8>)>, String> {
    match object.get(field) {
        None | Some(serde_json::Value::Null) => Ok(Vec::new()),
        Some(value) => parse_pairs(value, field),
    }
}

fn parse_pairs(value: &serde_json::Value, field: &str) -> Result<Vec<(String, Vec<u8>)>, String> {
    match value {
        serde_json::Value::Object(map) => {
            let mut pairs = Vec::with_capacity(map.len());
            for (key, item) in map {
                if key.is_empty() {
                    return Err(format!("{field} contains an empty key"));
                }
                let text = item
                    .as_str()
                    .ok_or_else(|| format!("{field}.{key} must be a string"))?;
                pairs.push((key.clone(), text.as_bytes().to_vec()));
            }
            Ok(pairs)
        }
        serde_json::Value::Array(items) => {
            let mut pairs = Vec::with_capacity(items.len());
            for (index, item) in items.iter().enumerate() {
                let Some(pair) = item.as_array() else {
                    return Err(format!("{field}[{index}] must be a [key, value] pair"));
                };
                if pair.len() != 2 {
                    return Err(format!("{field}[{index}] must be a [key, value] pair"));
                }
                let key = pair[0]
                    .as_str()
                    .ok_or_else(|| format!("{field}[{index}] key must be a string"))?;
                let text = pair[1]
                    .as_str()
                    .ok_or_else(|| format!("{field}[{index}] value must be a string"))?;
                if key.is_empty() {
                    return Err(format!("{field}[{index}] contains an empty key"));
                }
                pairs.push((key.to_string(), text.as_bytes().to_vec()));
            }
            Ok(pairs)
        }
        _ => Err(format!(
            "{field} must be an object or an array of [key, value] pairs"
        )),
    }
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

    #[test]
    fn seed_directory_loads_one_json_file_per_seed_and_skips_the_rest() {
        let dir = std::env::temp_dir().join(format!("atropos-seed-dir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("b-posts.json"),
            r#"{"method":"post","path":"/wp-json/batch/v1","query":{"preview":"1"},"headers":[["X-Test","yes"]],"body":{"title":"seed"}}"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("a-empty.json"),
            r#"{"method":"GET","path":"/wp-json/wp/v2/posts","body":null,"pin_route":false,"exec_limit":3}"#,
        )
        .unwrap();
        std::fs::write(dir.join("notes.txt"), "not a seed").unwrap();
        std::fs::write(dir.join("bad.json"), r#"{"method":"POST"}"#).unwrap();
        std::fs::write(
            dir.join(".hidden.json"),
            r#"{"method":"GET","path":"/","body":{}}"#,
        )
        .unwrap();

        let seeds = load_seed_directory(&dir).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();

        assert_eq!(seeds.len(), 2);
        assert_eq!(seeds[0].method, "GET");
        assert_eq!(seeds[0].path, "/wp-json/wp/v2/posts");
        assert_eq!(seeds[0].body, JsonValue::Null);
        assert!(!seeds[0].pin_route);
        assert_eq!(seeds[0].exec_limit, 3);
        assert_eq!(
            seeds[0].operation_key.as_deref(),
            Some("GET /wp-json/wp/v2/posts")
        );
        assert_eq!(seeds[1].method, "POST");
        assert_eq!(seeds[1].query, vec![("preview".to_string(), b"1".to_vec())]);
        assert_eq!(
            seeds[1].headers,
            vec![("X-Test".to_string(), b"yes".to_vec())]
        );
        assert!(seeds[1].pin_route);
        let body = String::from_utf8(seeds[1].body_bytes()).unwrap();
        assert!(body.contains("\"title\":\"seed\""));

        let tokens = dictionary_tokens(&seeds);
        for expected in [
            "GET",
            "POST",
            "/wp-json/wp/v2/posts",
            "/wp-json/batch/v1",
            "preview",
            "1",
            "X-Test",
            "yes",
            "title",
            "seed",
        ] {
            assert!(
                tokens.iter().any(|token| token == expected),
                "missing {expected} in {tokens:?}"
            );
        }
        assert_eq!(tokens.iter().filter(|token| *token == "POST").count(), 1);
    }

    #[test]
    fn dictionary_tokens_include_nested_keys_and_scalar_values() {
        let seed = parse_seed_json(
            r#"{"method":"POST","path":"/wp/v2/posts","body":{"title":"seed","count":2,"ok":true,"tags":["a",""]}}"#,
        )
        .unwrap();
        let tokens = dictionary_tokens(&[seed]);
        for expected in [
            "POST",
            "/wp/v2/posts",
            "title",
            "seed",
            "count",
            "2",
            "ok",
            "true",
            "tags",
            "a",
            "POST /wp/v2/posts",
        ] {
            assert!(
                tokens.iter().any(|token| token == expected),
                "missing {expected} in {tokens:?}"
            );
        }
        assert!(tokens.iter().all(|token| !token.is_empty()));
        assert_eq!(tokens.iter().filter(|token| *token == "POST").count(), 1);
    }

    #[test]
    fn seed_directory_rejects_a_directory_with_no_valid_json() {
        let dir = std::env::temp_dir().join(format!("atropos-seed-empty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("bad.json"),
            r#"{"path":"/wp/v2/posts?x=1","method":"POST","body":{}}"#,
        )
        .unwrap();

        let error = load_seed_directory(&dir).unwrap_err();
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(error.contains("no JSON file in the seed format"), "{error}");
        assert!(error.contains("path must start with /"), "{error}");
    }

    #[test]
    fn to_file_keeps_postcard_and_writes_a_plaintext_json_copy() {
        let dir = std::env::temp_dir().join(format!("atropos-corpus-json-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let seed = HttpInput::batch_seed();
        seed.to_file(dir.join("case")).unwrap();
        assert_eq!(HttpInput::from_file(dir.join("case")).unwrap(), seed);

        let text = std::fs::read_to_string(dir.join("case.json")).unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["method"], "POST");
        assert_eq!(value["path"], "/wp-json/batch/v1");
        assert_eq!(value["body"]["validation"], "normal");
        assert_eq!(value["body"]["requests"][0]["path"], "/wp/v2/posts");
        assert_eq!(value["body"]["requests"][0]["body"]["title"], "seed");
        assert!(value["body_override"].is_null());

        let mut smashed = seed;
        smashed.body_override = Some(b"not json".to_vec());
        smashed.headers = vec![
            ("X-Test".to_string(), b"a".to_vec()),
            ("X-Test".to_string(), b"b".to_vec()),
        ];
        smashed.to_file(dir.join("smashed")).unwrap();
        let smashed_text = std::fs::read_to_string(dir.join("smashed.json")).unwrap();
        let smashed_value: serde_json::Value = serde_json::from_str(&smashed_text).unwrap();
        assert_eq!(smashed_value["body_override"], "not json");
        assert_eq!(
            smashed_value["headers"],
            serde_json::json!([["X-Test", "a"], ["X-Test", "b"]])
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn nyx_payload_has_complete_json_and_a_terminal_nul() {
        let payload = HttpInput::batch_seed().nyx_payload();
        assert_eq!(payload.last(), Some(&0));

        let parsed: serde_json::Value =
            serde_json::from_slice(&payload[..payload.len() - 1]).unwrap();
        assert!(parsed["requests"].is_array());
    }

    #[test]
    fn value_paths_include_root_and_nested_nodes() {
        let value = JsonValue::Object(vec![(
            "items".to_string(),
            JsonValue::Array(vec![
                JsonValue::String(b"first".to_vec()),
                JsonValue::Object(vec![("enabled".to_string(), JsonValue::Bool(true))]),
            ]),
        )]);

        assert_eq!(
            value.value_paths(),
            vec![vec![], vec![0], vec![0, 0], vec![0, 1], vec![0, 1, 0]]
        );
    }

    #[test]
    fn array_paths_and_mutable_items_reach_nested_arrays() {
        let mut value = JsonValue::Object(vec![(
            "items".to_string(),
            JsonValue::Array(vec![JsonValue::Array(vec![JsonValue::Null])]),
        )]);

        assert_eq!(value.array_paths(), vec![vec![0], vec![0, 0]]);
        value
            .array_items_mut(&[0, 0])
            .unwrap()
            .push(JsonValue::Bool(true));
        assert_eq!(
            value.leaf_ref(&[0, 0]),
            Some(&JsonValue::Array(vec![
                JsonValue::Null,
                JsonValue::Bool(true)
            ]))
        );
    }
}
