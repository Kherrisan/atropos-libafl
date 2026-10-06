use std::{fs, path::Path};

use std::collections::BTreeMap;

use roas::common::bool_or::BoolOr;
use roas::common::formats::SchemaType;
use roas::common::reference::{RefOr, ReferenceObject};
use roas::v3_2::media_type::MediaType;
use roas::v3_2::parameter::Parameter as OasParameter;
use roas::v3_2::path_item::PathItem;
use roas::v3_2::request_body::RequestBody;
use roas::v3_2::schema::{
    ArraySchema, IntegerSchema, NumberSchema as OasNumberSchema, ObjectSchema, Schema as OasSchema,
    SchemaRef, SingleSchema, StringSchema,
};
use roas::v3_2::spec::Spec;

use crate::input::{HttpInput, JsonValue};

const STRING_BOUNDARY_CAP: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnumStyle {
    Text,
    Number,
    /// Boolean schemas in OpenAPI 3.1 do not carry an enum list, so this
    /// style is reserved for a bool-valued enum if one is projected later.
    #[allow(dead_code)]
    Bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Field {
    pub name: String,
    pub required: bool,
    pub schema: Schema,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Schema {
    Any {
        nullable: bool,
    },
    Enum {
        values: Vec<String>,
        nullable: bool,
        style: EnumStyle,
    },
    Object {
        fields: Vec<Field>,
        nullable: bool,
    },
    Array {
        items: Box<Schema>,
        min_items: Option<usize>,
        max_items: Option<usize>,
        nullable: bool,
    },
    String {
        min_length: Option<usize>,
        max_length: Option<usize>,
        nullable: bool,
    },
    Number {
        integer: bool,
        minimum: Option<f64>,
        maximum: Option<f64>,
        nullable: bool,
    },
    Bool {
        nullable: bool,
    },
    Union {
        variants: Vec<Schema>,
        nullable: bool,
    },
}

impl Schema {
    fn nullable(&self) -> bool {
        match self {
            Self::Any { nullable }
            | Self::Enum { nullable, .. }
            | Self::Object { nullable, .. }
            | Self::Array { nullable, .. }
            | Self::String { nullable, .. }
            | Self::Number { nullable, .. }
            | Self::Bool { nullable }
            | Self::Union { nullable, .. } => *nullable,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParameterLocation {
    Query,
    Header,
    Cookie,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Parameter {
    pub name: String,
    pub required: bool,
    pub location: ParameterLocation,
    pub schema: Schema,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Operation {
    pub method: String,
    pub path_template: String,
    pub body: Option<Schema>,
    pub parameters: Vec<Parameter>,
    pub key: String,
}

impl Operation {
    pub fn to_input(&self) -> HttpInput {
        let body = self
            .body
            .as_ref()
            .map(example_json)
            .unwrap_or(JsonValue::Object(Vec::new()));
        HttpInput::from_request(crate::input::HttpRequest {
            method: self.method.clone(),
            path: self.path_template.clone(),
            query: Vec::new(),
            headers: Vec::new(),
            cookies: Vec::new(),
            body,
            body_override: None,
            operation_key: Some(self.key.clone()),
            pin_route: true,
        })
    }
}

pub fn load_operation_files(paths: &[impl AsRef<Path>]) -> Result<Vec<Operation>, String> {
    let mut merged = Vec::new();
    for path in paths {
        let path = path.as_ref();
        let text = fs::read_to_string(path).map_err(|err| format!("{}: {err}", path.display()))?;
        let spec = parse_spec(&text).map_err(|err| format!("{}: {err}", path.display()))?;
        for operation in operations_from_spec(&spec)? {
            if let Some(index) = merged
                .iter()
                .position(|item: &Operation| item.key == operation.key)
            {
                eprintln!(
                    "openapi: {} from {} replaces an earlier definition",
                    operation.key,
                    path.display()
                );
                merged[index] = operation;
            } else {
                merged.push(operation);
            }
        }
    }
    Ok(merged)
}

pub fn match_operation<'a>(
    input: &crate::input::HttpRequest,
    operations: &'a [Operation],
) -> Option<&'a Operation> {
    if let Some(key) = &input.operation_key {
        if let Some(operation) = operations.iter().find(|operation| &operation.key == key) {
            return Some(operation);
        }
    }
    let method = input.method.to_ascii_uppercase();
    if let Some(operation) = operations
        .iter()
        .find(|operation| operation.method == method && operation.path_template == input.path)
    {
        return Some(operation);
    }
    operations.iter().find(|operation| {
        operation.method == method && path_matches(&operation.path_template, &input.path)
    })
}

pub fn peel_union<'a>(schema: &'a Schema, value: &JsonValue) -> Option<&'a Schema> {
    let mut schema = schema;
    for _ in 0..8 {
        match schema {
            Schema::Union { variants, .. } => {
                if variants.is_empty() {
                    return None;
                }
                schema = variants
                    .iter()
                    .find(|variant| schema_accepts(variant, value))
                    .unwrap_or(&variants[0]);
            }
            _ => return Some(schema),
        }
    }
    None
}

fn parse_spec(text: &str) -> Result<Spec, String> {
    let yaml: serde_yaml::Value = serde_yaml::from_str(text).map_err(|err| err.to_string())?;
    let json = serde_json::to_value(yaml).map_err(|err| err.to_string())?;
    let version = json
        .get("openapi")
        .or_else(|| json.get("swagger"))
        .and_then(|value| value.as_str())
        .ok_or_else(|| "missing openapi or swagger version".to_string())?;
    if version.starts_with("2.") {
        let spec: roas::v2::spec::Spec =
            serde_json::from_value(json).map_err(|err| err.to_string())?;
        let spec = roas::v3_0::spec::Spec::from(spec);
        let spec = roas::v3_1::spec::Spec::from(spec);
        return Ok(Spec::from(spec));
    }
    if version.starts_with("3.0") {
        let spec: roas::v3_0::spec::Spec =
            serde_json::from_value(json).map_err(|err| err.to_string())?;
        let spec = roas::v3_1::spec::Spec::from(spec);
        return Ok(Spec::from(spec));
    }
    if version.starts_with("3.1") {
        let spec: roas::v3_1::spec::Spec =
            serde_json::from_value(json).map_err(|err| err.to_string())?;
        return Ok(Spec::from(spec));
    }
    if version.starts_with("3.2") {
        return serde_json::from_value(json).map_err(|err| err.to_string());
    }
    Err(format!(
        "unsupported OpenAPI version {version}; expected 2.0, 3.0, 3.1, or 3.2"
    ))
}

fn operations_from_spec(spec: &Spec) -> Result<Vec<Operation>, String> {
    let mut operations = Vec::new();
    let Some(paths) = &spec.paths else {
        return Ok(operations);
    };
    for (template, item) in &paths.paths {
        let mut seen_paths = Vec::new();
        let item = concrete_path_item(spec, item, &mut seen_paths)?;
        let Some(methods) = &item.operations else {
            continue;
        };
        for (method, operation) in methods {
            let method = method.to_ascii_uppercase();
            let mut seen = Vec::new();
            let body = match &operation.request_body {
                Some(body) => request_body_schema(spec, body, &mut seen)?,
                None => None,
            };
            let mut parameters = Vec::new();
            let path_parameters = item.parameters.iter().flatten();
            let operation_parameters = operation.parameters.iter().flatten();
            for parameter in path_parameters.chain(operation_parameters) {
                if let Some(projected) = project_parameter(spec, parameter, &mut seen)? {
                    if let Some(index) = parameters.iter().position(|existing: &Parameter| {
                        existing.location == projected.location && existing.name == projected.name
                    }) {
                        parameters[index] = projected;
                    } else {
                        parameters.push(projected);
                    }
                }
            }
            let key = format!("{method} {template}");
            operations.push(Operation {
                method,
                path_template: template.clone(),
                body,
                parameters,
                key,
            });
        }
    }
    Ok(operations)
}

fn concrete_path_item<'a>(
    spec: &'a Spec,
    item: &'a PathItem,
    seen: &mut Vec<String>,
) -> Result<&'a PathItem, String> {
    let Some(reference) = &item.reference else {
        return Ok(item);
    };
    if seen.iter().any(|item| item == reference) {
        return Err(format!("cycle in path $ref {reference}"));
    }
    seen.push(reference.clone());
    let resolved = roas::common::reference::ResolveReference::resolve_reference(spec, reference)
        .ok_or_else(|| format!("unresolved path $ref {reference}"))?;
    concrete_path_item(spec, resolved, seen)
}

fn request_body_schema(
    spec: &Spec,
    body: &RefOr<RequestBody>,
    seen: &mut Vec<String>,
) -> Result<Option<Schema>, String> {
    let body = body
        .get_item(spec)
        .map_err(|err| format!("requestBody: {err}"))?;
    let Some((_, media)) = body.content.iter().find(|(name, _)| {
        name.as_str() == "application/json" || name.as_str().starts_with("application/json")
    }) else {
        return Ok(None);
    };
    let media = match media {
        RefOr::Item(media) => media,
        RefOr::Ref(reference) => {
            return Err(format!(
                "unsupported media type $ref {}",
                reference.reference()
            ));
        }
    };
    match &media.schema {
        Some(schema) => Ok(Some(resolve_schema(spec, schema, seen)?)),
        None => Ok(None),
    }
}

fn project_parameter(
    spec: &Spec,
    parameter: &RefOr<OasParameter>,
    seen: &mut Vec<String>,
) -> Result<Option<Parameter>, String> {
    let parameter = parameter
        .get_item(spec)
        .map_err(|err| format!("parameter: {err}"))?;
    let Some(view) = parameter_view(parameter) else {
        return Ok(None);
    };
    if view.location == ParameterLocation::Header
        && matches!(
            view.name.to_ascii_lowercase().as_str(),
            "accept" | "content-type" | "authorization"
        )
    {
        return Ok(None);
    }
    let schema = if let Some(schema) = view.schema {
        resolve_schema(spec, schema, seen)?
    } else if let Some(content) = view.content {
        schema_from_content(spec, content, seen)?
    } else {
        Schema::Any { nullable: false }
    };
    Ok(Some(Parameter {
        name: view.name.to_string(),
        required: view.required,
        location: view.location,
        schema,
    }))
}

struct ParameterView<'a> {
    name: &'a str,
    required: bool,
    location: ParameterLocation,
    schema: Option<&'a RefOr<OasSchema, SchemaRef>>,
    content: Option<&'a BTreeMap<String, RefOr<MediaType>>>,
}

fn parameter_view(parameter: &OasParameter) -> Option<ParameterView<'_>> {
    match parameter {
        OasParameter::Query(parameter) => Some(ParameterView {
            name: &parameter.name,
            required: parameter.required.unwrap_or(false),
            location: ParameterLocation::Query,
            schema: parameter.schema.as_ref(),
            content: parameter.content.as_ref(),
        }),
        OasParameter::Header(parameter) => Some(ParameterView {
            name: &parameter.name,
            required: parameter.required.unwrap_or(false),
            location: ParameterLocation::Header,
            schema: parameter.schema.as_ref(),
            content: parameter.content.as_ref(),
        }),
        OasParameter::Cookie(parameter) => Some(ParameterView {
            name: &parameter.name,
            required: parameter.required.unwrap_or(false),
            location: ParameterLocation::Cookie,
            schema: parameter.schema.as_ref(),
            content: parameter.content.as_ref(),
        }),
        OasParameter::Path(_) | OasParameter::Querystring(_) => None,
    }
}

fn schema_from_content(
    spec: &Spec,
    content: &BTreeMap<String, RefOr<MediaType>>,
    seen: &mut Vec<String>,
) -> Result<Schema, String> {
    let Some((_, media)) = content.iter().find(|(name, _)| {
        name.as_str() == "application/json" || name.as_str().starts_with("application/json")
    }) else {
        return Ok(Schema::Any { nullable: false });
    };
    let RefOr::Item(media) = media else {
        return Ok(Schema::Any { nullable: false });
    };
    match &media.schema {
        Some(schema) => resolve_schema(spec, schema, seen),
        None => Ok(Schema::Any { nullable: false }),
    }
}

fn resolve_schema(
    spec: &Spec,
    schema: &RefOr<OasSchema, SchemaRef>,
    seen: &mut Vec<String>,
) -> Result<Schema, String> {
    match schema {
        RefOr::Item(schema) => project_schema(spec, schema, seen),
        RefOr::Ref(reference) => follow_schema(spec, reference.reference(), seen),
    }
}

fn follow_schema(spec: &Spec, reference: &str, seen: &mut Vec<String>) -> Result<Schema, String> {
    if seen.iter().any(|item| item == reference) {
        return Ok(Schema::Any { nullable: false });
    }
    let name = reference
        .strip_prefix("#/components/schemas/")
        .ok_or_else(|| format!("unsupported schema $ref {reference}"))?;
    let name = unescape_pointer(name);
    seen.push(reference.to_string());
    let next = spec
        .components
        .as_ref()
        .and_then(|components| components.schemas.as_ref())
        .and_then(|schemas| schemas.get(&name))
        .ok_or_else(|| format!("unresolved schema $ref {reference}"))?;
    let projected = resolve_schema(spec, next, seen)?;
    seen.pop();
    Ok(projected)
}

fn project_schema(
    spec: &Spec,
    schema: &OasSchema,
    seen: &mut Vec<String>,
) -> Result<Schema, String> {
    match schema {
        OasSchema::Bool(true) | OasSchema::Empty(_) => Ok(Schema::Any { nullable: false }),
        OasSchema::Bool(false) | OasSchema::Not(_) => Ok(Schema::Any { nullable: false }),
        OasSchema::Single(single) => match single.as_ref() {
            SingleSchema::String(typed) => Ok(string_schema(typed)),
            SingleSchema::Number(typed) => Ok(number_schema(false, typed)),
            SingleSchema::Integer(typed) => Ok(integer_schema(typed)),
            SingleSchema::Boolean(_) => Ok(Schema::Bool { nullable: false }),
            SingleSchema::Object(typed) => object_schema(spec, typed, false, seen),
            SingleSchema::Array(typed) => array_schema(spec, typed, false, seen),
            SingleSchema::Null(_) => Ok(Schema::Any { nullable: true }),
        },
        OasSchema::Multi(typed) => multi_schema(typed),
        OasSchema::OneOf(typed) => union_schema(spec, &typed.one_of, false, seen),
        OasSchema::AnyOf(typed) => union_schema(spec, &typed.any_of, false, seen),
        OasSchema::AllOf(typed) => all_of_schema(spec, &typed.all_of, false, seen),
    }
}

fn string_schema(typed: &StringSchema) -> Schema {
    let values = typed.enum_values.clone().unwrap_or_default();
    if values.is_empty() {
        Schema::String {
            min_length: as_usize(typed.min_length),
            max_length: as_usize(typed.max_length),
            nullable: false,
        }
    } else {
        Schema::Enum {
            values,
            nullable: false,
            style: EnumStyle::Text,
        }
    }
}

fn integer_schema(typed: &IntegerSchema) -> Schema {
    let minimum = bound(
        typed.minimum.as_ref(),
        typed.exclusive_minimum.as_ref(),
        true,
        true,
    );
    let maximum = bound(
        typed.maximum.as_ref(),
        typed.exclusive_maximum.as_ref(),
        true,
        false,
    );
    let values = typed
        .enum_values
        .iter()
        .flatten()
        .map(|value| value.to_string())
        .collect::<Vec<_>>();
    if values.is_empty() {
        Schema::Number {
            integer: true,
            minimum,
            maximum,
            nullable: false,
        }
    } else {
        Schema::Enum {
            values,
            nullable: false,
            style: EnumStyle::Number,
        }
    }
}

fn number_schema(integer: bool, typed: &OasNumberSchema) -> Schema {
    let minimum = bound(
        typed.minimum.as_ref(),
        typed.exclusive_minimum.as_ref(),
        integer,
        true,
    );
    let maximum = bound(
        typed.maximum.as_ref(),
        typed.exclusive_maximum.as_ref(),
        integer,
        false,
    );
    let values = typed
        .enum_values
        .iter()
        .flatten()
        .map(|value| value.to_string())
        .collect::<Vec<_>>();
    if values.is_empty() {
        Schema::Number {
            integer,
            minimum,
            maximum,
            nullable: false,
        }
    } else {
        Schema::Enum {
            values,
            nullable: false,
            style: EnumStyle::Number,
        }
    }
}

fn object_schema(
    spec: &Spec,
    typed: &ObjectSchema,
    nullable: bool,
    seen: &mut Vec<String>,
) -> Result<Schema, String> {
    let required = typed.required.as_deref().unwrap_or(&[]);
    let mut fields = Vec::new();
    for (name, schema) in typed.properties.iter().flatten() {
        fields.push(Field {
            name: name.clone(),
            required: required.iter().any(|item| item == name),
            schema: resolve_schema(spec, schema, seen)?,
        });
    }
    Ok(Schema::Object { fields, nullable })
}

fn array_schema(
    spec: &Spec,
    typed: &ArraySchema,
    nullable: bool,
    seen: &mut Vec<String>,
) -> Result<Schema, String> {
    let items = match &typed.items {
        Some(BoolOr::Item(schema)) => resolve_schema(spec, schema, seen)?,
        Some(BoolOr::Bool(_)) | None => Schema::Any { nullable: false },
    };
    Ok(Schema::Array {
        items: Box::new(items),
        min_items: as_usize(typed.min_items),
        max_items: as_usize(typed.max_items),
        nullable,
    })
}

fn union_schema(
    spec: &Spec,
    parts: &[RefOr<OasSchema, SchemaRef>],
    nullable: bool,
    seen: &mut Vec<String>,
) -> Result<Schema, String> {
    let mut variants = Vec::new();
    for part in parts {
        variants.push(resolve_schema(spec, part, seen)?);
    }
    Ok(Schema::Union { variants, nullable })
}

fn all_of_schema(
    spec: &Spec,
    parts: &[RefOr<OasSchema, SchemaRef>],
    mut nullable: bool,
    seen: &mut Vec<String>,
) -> Result<Schema, String> {
    let mut fields: Vec<Field> = Vec::new();
    let mut others = Vec::new();
    for part in parts {
        match resolve_schema(spec, part, seen)? {
            Schema::Object {
                fields: child,
                nullable: child_nullable,
            } => {
                nullable |= child_nullable;
                for field in child {
                    if let Some(existing) = fields.iter_mut().find(|item| item.name == field.name) {
                        *existing = field;
                    } else {
                        fields.push(field);
                    }
                }
            }
            other => {
                nullable |= other.nullable();
                others.push(other);
            }
        }
    }
    if others.is_empty() {
        return Ok(Schema::Object { fields, nullable });
    }
    if !fields.is_empty() {
        others.insert(
            0,
            Schema::Object {
                fields,
                nullable: false,
            },
        );
    }
    if others.len() == 1 {
        let mut only = others.remove(0);
        if let Schema::Object {
            nullable: ref mut flag,
            ..
        } = only
        {
            *flag |= nullable;
        }
        return Ok(only);
    }
    Ok(Schema::Union {
        variants: others,
        nullable,
    })
}

fn multi_schema(typed: &roas::v3_2::schema::MultiSchema) -> Result<Schema, String> {
    let nullable = typed
        .schema_types
        .iter()
        .any(|kind| matches!(kind, SchemaType::Null));
    let kinds = typed
        .schema_types
        .iter()
        .filter(|kind| !matches!(kind, SchemaType::Null))
        .collect::<Vec<_>>();
    let mut variants = Vec::new();
    for kind in kinds {
        variants.push(match kind {
            SchemaType::String => Schema::String {
                min_length: None,
                max_length: None,
                nullable,
            },
            SchemaType::Integer => Schema::Number {
                integer: true,
                minimum: None,
                maximum: None,
                nullable,
            },
            SchemaType::Number => Schema::Number {
                integer: false,
                minimum: None,
                maximum: None,
                nullable,
            },
            SchemaType::Boolean => Schema::Bool { nullable },
            SchemaType::Array | SchemaType::Object | SchemaType::Custom(_) | SchemaType::Null => {
                Schema::Any { nullable }
            }
        });
    }
    match variants.len() {
        0 => Ok(Schema::Any { nullable }),
        1 => Ok(variants.remove(0)),
        _ => Ok(Schema::Union { variants, nullable }),
    }
}

fn as_usize(value: Option<u64>) -> Option<usize> {
    value.and_then(|value| usize::try_from(value).ok())
}

fn bound(
    inclusive: Option<&serde_json::Number>,
    exclusive: Option<&serde_json::Number>,
    integer: bool,
    lower: bool,
) -> Option<f64> {
    let inclusive = inclusive
        .and_then(|number| number.as_f64())
        .filter(|value| value.is_finite());
    let exclusive = exclusive
        .and_then(|number| number.as_f64())
        .filter(|value| value.is_finite())
        .map(|value| {
            if !integer {
                value
            } else if lower {
                value.floor() + 1.0
            } else {
                value.ceil() - 1.0
            }
        });
    match (inclusive, exclusive) {
        (Some(inclusive), Some(exclusive)) if lower => Some(inclusive.max(exclusive)),
        (Some(inclusive), Some(exclusive)) => Some(inclusive.min(exclusive)),
        (Some(inclusive), None) => Some(inclusive),
        (None, Some(exclusive)) => Some(exclusive),
        (None, None) => None,
    }
}
fn unescape_pointer(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '~' {
            match chars.next() {
                Some('0') => out.push('~'),
                Some('1') => out.push('/'),
                Some(other) => {
                    out.push('~');
                    out.push(other);
                }
                None => out.push('~'),
            }
        } else {
            out.push(ch);
        }
    }
    out
}

fn path_matches(template: &str, path: &str) -> bool {
    let template: Vec<_> = split_path(template);
    let path: Vec<_> = split_path(path);
    template.len() == path.len()
        && template.iter().zip(path).all(|(expected, actual)| {
            (expected.starts_with('{') && expected.ends_with('}')) || expected == &actual
        })
}

fn split_path(path: &str) -> Vec<&str> {
    path.split('/')
        .filter(|segment| !segment.is_empty())
        .collect()
}

pub(crate) fn schema_at<'a>(
    schema: &'a Schema,
    path: &[usize],
    body: &JsonValue,
) -> Option<&'a Schema> {
    let mut schema = schema;
    let mut node = body;
    for index in path {
        schema = peel_union(schema, node)?;
        match (schema, node) {
            (Schema::Object { fields, .. }, JsonValue::Object(values)) => {
                let (name, child) = values.get(*index)?;
                schema = &fields.iter().find(|field| &field.name == name)?.schema;
                node = child;
            }
            (Schema::Array { items, .. }, JsonValue::Array(values)) => {
                schema = items.as_ref();
                node = values.get(*index)?;
            }
            _ => return None,
        }
    }
    peel_union(schema, node)
}

fn schema_accepts(schema: &Schema, value: &JsonValue) -> bool {
    if matches!(value, JsonValue::Null) && schema.nullable() {
        return true;
    }
    match schema {
        Schema::Any { .. } => true,
        Schema::Enum { values, style, .. } => enum_current(*style, value)
            .is_some_and(|text| values.iter().any(|value| value == &text)),
        Schema::Object { .. } => matches!(value, JsonValue::Object(_)),
        Schema::Array { .. } => matches!(value, JsonValue::Array(_)),
        Schema::String { .. } => matches!(value, JsonValue::String(_)),
        Schema::Number { .. } => matches!(value, JsonValue::Number(_)),
        Schema::Bool { .. } => matches!(value, JsonValue::Bool(_)),
        Schema::Union { variants, .. } => variants
            .iter()
            .any(|variant| schema_accepts(variant, value)),
    }
}

pub(crate) fn enum_current(style: EnumStyle, value: &JsonValue) -> Option<String> {
    match (style, value) {
        (EnumStyle::Text, JsonValue::String(bytes)) => {
            Some(String::from_utf8_lossy(bytes).into_owned())
        }
        (EnumStyle::Number, JsonValue::Number(text)) => Some(text.clone()),
        (EnumStyle::Bool, JsonValue::Bool(true)) => Some("true".to_string()),
        (EnumStyle::Bool, JsonValue::Bool(false)) => Some("false".to_string()),
        _ => None,
    }
}

pub(crate) fn enum_json(style: EnumStyle, choice: &str) -> JsonValue {
    match style {
        EnumStyle::Text => JsonValue::String(choice.as_bytes().to_vec()),
        EnumStyle::Number => JsonValue::Number(choice.to_string()),
        EnumStyle::Bool => JsonValue::Bool(choice == "true"),
    }
}

pub(crate) fn boundary_numbers(
    integer: bool,
    minimum: Option<f64>,
    maximum: Option<f64>,
    current: &str,
) -> Vec<String> {
    let mut raw = Vec::new();
    if let Some(minimum) = minimum {
        raw.push(minimum);
        raw.push(minimum + 1.0);
    }
    if let Some(maximum) = maximum {
        raw.push(maximum);
        raw.push(maximum - 1.0);
    }
    raw.extend([0.0, 1.0, -1.0]);
    let mut out = Vec::new();
    for value in raw {
        let Some(text) = format_number(value, integer) else {
            continue;
        };
        if text == current || !number_in_range(minimum, maximum, integer, &text) {
            continue;
        }
        if out.iter().any(|existing| existing == &text) {
            continue;
        }
        out.push(text);
        if out.len() == 6 {
            break;
        }
    }
    out
}

pub(crate) fn number_in_range(
    minimum: Option<f64>,
    maximum: Option<f64>,
    integer: bool,
    text: &str,
) -> bool {
    if integer && (text.contains('.') || text.contains('e') || text.contains('E')) {
        return false;
    }
    let Ok(value) = text.parse::<f64>() else {
        return false;
    };
    if !value.is_finite() {
        return false;
    }
    if integer && text.parse::<i128>().is_err() {
        return false;
    }
    if minimum.is_some_and(|minimum| value < minimum)
        || maximum.is_some_and(|maximum| value > maximum)
    {
        return false;
    }
    true
}

fn format_number(value: f64, integer: bool) -> Option<String> {
    if !value.is_finite() {
        return None;
    }
    if integer || value.fract() == 0.0 {
        if value.abs() > i64::MAX as f64 {
            return None;
        }
        return Some(format!("{}", value.round() as i64));
    }
    Some(value.to_string())
}

fn length_boundary(min_length: Option<usize>, max_length: Option<usize>) -> usize {
    let min_length = min_length.unwrap_or(0).min(STRING_BOUNDARY_CAP);
    match max_length {
        Some(max_length) => max_length.min(STRING_BOUNDARY_CAP).max(min_length),
        None if min_length == 0 => STRING_BOUNDARY_CAP,
        None => min_length,
    }
}

pub(crate) fn sample_value(schema: &Schema) -> JsonValue {
    if let Schema::Union { variants, .. } = schema {
        return variants
            .first()
            .map(sample_value)
            .unwrap_or(JsonValue::Null);
    }
    match schema {
        Schema::Enum { values, style, .. } => values
            .first()
            .map(|choice| enum_json(*style, choice))
            .unwrap_or(JsonValue::String(Vec::new())),
        Schema::Bool { .. } => JsonValue::Bool(false),
        Schema::Number {
            integer,
            minimum,
            maximum,
            ..
        } => JsonValue::Number(sample_number(*integer, *minimum, *maximum)),
        Schema::String {
            min_length,
            max_length,
            ..
        } => JsonValue::String(vec![
            b'a';
            length_boundary(*min_length, *max_length)
                .min(min_length.unwrap_or(0).max(0))
        ]),
        Schema::Array {
            items, min_items, ..
        } => {
            let count = min_items.unwrap_or(1).max(1);
            JsonValue::Array((0..count).map(|_| sample_value(items)).collect())
        }
        Schema::Object { fields, .. } => JsonValue::Object(
            fields
                .iter()
                .filter(|field| field.required)
                .map(|field| (field.name.clone(), sample_value(&field.schema)))
                .collect(),
        ),
        Schema::Any { .. } | Schema::Union { .. } => JsonValue::String(Vec::new()),
    }
}

fn sample_number(integer: bool, minimum: Option<f64>, maximum: Option<f64>) -> String {
    for value in [minimum, Some(0.0), maximum, Some(1.0)]
        .into_iter()
        .flatten()
    {
        if let Some(text) = format_number(value, integer) {
            if number_in_range(minimum, maximum, integer, &text) {
                return text;
            }
        }
    }
    "0".to_string()
}

fn example_json(schema: &Schema) -> JsonValue {
    match schema {
        Schema::Enum { values, style, .. } => values
            .first()
            .map(|choice| enum_json(*style, choice))
            .unwrap_or(JsonValue::String(Vec::new())),
        Schema::Object { fields, .. } => JsonValue::Object(
            fields
                .iter()
                .map(|field| (field.name.clone(), example_json(&field.schema)))
                .collect(),
        ),
        Schema::Array { items, .. } => JsonValue::Array(vec![example_json(items)]),
        Schema::Bool { .. } => JsonValue::Bool(false),
        Schema::Number {
            integer,
            minimum,
            maximum,
            ..
        } => JsonValue::Number(sample_number(*integer, *minimum, *maximum)),
        Schema::String { .. } | Schema::Any { .. } => JsonValue::String(Vec::new()),
        Schema::Union { variants, .. } => variants
            .first()
            .map(example_json)
            .unwrap_or(JsonValue::Object(Vec::new())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn write_spec(name: &str, text: &str) -> std::path::PathBuf {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "atropos-openapi-{name}-{}-{timestamp}.yaml",
            std::process::id()
        ));
        std::fs::write(&path, text).unwrap();
        path
    }

    #[test]
    fn loads_batch_operation() {
        let path = write_spec(
            "batch",
            r#"
openapi: 3.0.0
info:
  title: batch
  version: "1.0.0"
paths:
  /wp-json/batch/v1:
    post:
      responses:
        "200":
          description: ok
      requestBody:
        content:
          application/json:
            schema:
              type: object
              properties:
                validation:
                  type: string
                requests:
                  type: array
                  items:
                    type: object
                    properties:
                      method:
                        type: string
                      path:
                        type: string
                      body:
                        type: object
                        properties:
                          title:
                            type: string
                      headers:
                        type: object
"#,
        );
        let ops = load_operation_files(&[path.clone()]).unwrap();
        std::fs::remove_file(path).unwrap();
        assert_eq!(ops.len(), 1);
        let input = ops[0].to_input();
        let body = String::from_utf8(input.requests[0].body_bytes()).unwrap();
        assert!(body.contains("requests"));
        assert!(body.contains("title"));
        assert!(body.contains("method"));
        assert_eq!(input.requests[0].method, "POST");
        assert_eq!(input.requests[0].path, "/wp-json/batch/v1");
    }

    #[test]
    fn projects_refs_required_enums_and_numeric_bounds() {
        let path = write_spec(
            "schema",
            r##"
openapi: 3.0.0
info:
  title: schema
  version: "1.0.0"
paths:
  /items/{id}:
    post:
      parameters:
        - $ref: "#/components/parameters/Limit"
      responses:
        "200":
          description: ok
      requestBody:
        content:
          application/json:
            schema:
              allOf:
                - $ref: "#/components/schemas/Body"
components:
  parameters:
    Limit:
      name: limit
      in: query
      schema:
        type: integer
  schemas:
    Body:
      type: object
      required: [id]
      properties:
        id:
          type: integer
          minimum: 1
          maximum: 10
        title:
          $ref: "#/components/schemas/Title"
        status:
          type: string
          enum: [new, draft]
    Title:
      type: string
"##,
        );
        let ops = load_operation_files(&[path.clone()]).unwrap();
        std::fs::remove_file(path).unwrap();
        let body = ops[0].body.as_ref().unwrap();
        let Schema::Object { fields, .. } = body else {
            panic!("expected object, got {body:?}");
        };
        let id = fields.iter().find(|field| field.name == "id").unwrap();
        assert!(id.required);
        assert_eq!(
            id.schema,
            Schema::Number {
                integer: true,
                minimum: Some(1.0),
                maximum: Some(10.0),
                nullable: false,
            }
        );
        let title = fields.iter().find(|field| field.name == "title").unwrap();
        assert!(!title.required);
        assert!(matches!(title.schema, Schema::String { .. }));
        let status = fields.iter().find(|field| field.name == "status").unwrap();
        assert_eq!(
            status.schema,
            Schema::Enum {
                values: vec!["new".to_string(), "draft".to_string()],
                nullable: false,
                style: EnumStyle::Text,
            }
        );
        assert_eq!(ops[0].parameters.len(), 1);
        assert_eq!(ops[0].parameters[0].name, "limit");
        assert_eq!(ops[0].parameters[0].location, ParameterLocation::Query);
        let mut input = ops[0].to_input();
        input.requests[0].path = "/items/9".to_string();
        assert!(match_operation(&input.requests[0], &ops).is_some());
    }

    #[test]
    fn later_file_replaces_the_same_operation() {
        let first = write_spec(
            "first",
            r#"
openapi: 3.0.0
info:
  title: first
  version: "1.0.0"
paths:
  /x:
    post:
      responses:
        "200":
          description: ok
      requestBody:
        content:
          application/json:
            schema:
              type: object
              properties:
                old:
                  type: string
"#,
        );
        let second = write_spec(
            "second",
            r#"
openapi: 3.0.0
info:
  title: second
  version: "1.0.0"
paths:
  /x:
    post:
      responses:
        "200":
          description: ok
      requestBody:
        content:
          application/json:
            schema:
              type: object
              properties:
                new:
                  type: string
"#,
        );
        let ops = load_operation_files(&[first.clone(), second.clone()]).unwrap();
        std::fs::remove_file(first).unwrap();
        std::fs::remove_file(second).unwrap();
        assert_eq!(ops.len(), 1);
        let Schema::Object { fields, .. } = ops[0].body.as_ref().unwrap() else {
            panic!("expected object");
        };
        assert!(fields.iter().any(|field| field.name == "new"));
        assert!(fields.iter().all(|field| field.name != "old"));
    }

    #[test]
    fn loads_openapi_31_and_rejects_an_unknown_version() {
        let path = write_spec(
            "v31",
            r#"
openapi: 3.1.0
info:
  title: next
  version: "1.0.0"
paths:
  /wp-json/batch/v1:
    post:
      responses:
        "200":
          description: ok
      requestBody:
        content:
          application/json:
            schema:
              type: object
              required: [title]
              properties:
                title:
                  type: string
"#,
        );
        let ops = load_operation_files(&[path.clone()]).unwrap();
        std::fs::remove_file(path).unwrap();
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].method, "POST");
        let Schema::Object { fields, .. } = ops[0].body.as_ref().unwrap() else {
            panic!("expected object");
        };
        assert!(fields
            .iter()
            .any(|field| field.name == "title" && field.required));

        let path = write_spec(
            "v4",
            r#"
openapi: 4.0.0
info:
  title: future
  version: "1.0.0"
paths: {}
"#,
        );
        let error = load_operation_files(&[path.clone()]).unwrap_err();
        std::fs::remove_file(path).unwrap();
        assert!(error.contains("unsupported OpenAPI version"), "{error}");
    }
}
