use std::{borrow::Cow, env, fs};

use libafl::{
    corpus::{Corpus, CorpusId, HasCurrentCorpusId},
    inputs::HasTargetBytes,
    mutators::{MutationResult, Mutator, Tokens},
    state::{HasCorpus, HasRand},
    Error,
};
use libafl_bolts::{rands::Rand, Named};

use crate::{
    input::{HttpInput, JsonValue, NYX_INPUT_BUFFER_SIZE},
    openapi::{Operation, Schema},
};

const CANARIES: &[&[u8]] = &[
    b"' crash ",
    b"\" crash ",
    b"crash",
    b"crash.php",
    b"secret4815162342",
];

const NUMBER_EDGES: &[&str] = &[
    "0",
    "1",
    "-1",
    "127",
    "-128",
    "255",
    "256",
    "32767",
    "32768",
    "65535",
    "65536",
    "2147483647",
    "2147483648",
    "4294967295",
    "4294967296",
];

const NUMBER_DELTAS: &[i128] = &[1, 2, 4, 8, 16, 32, 35];

#[derive(Clone, Copy)]
enum MutationOp {
    Path,
    String,
    Number,
    Bool,
    NullOrType,
    ObjectKeyInsert,
    ObjectKeyRemove,
    ObjectKeyRename,
    Array,
    NestNewField,
    NestExistingField,
    HttpMetadata,
    Crossover,
}

pub struct DeterministicMutator {
    name: Cow<'static, str>,
    operations: Vec<Operation>,
    dictionary: Vec<String>,
    key_candidates: Vec<String>,
    violation_rate: f64,
}

impl DeterministicMutator {
    pub fn new(operations: Vec<Operation>) -> Result<Self, String> {
        let dictionary = load_dictionary_from_env()?;
        let key_candidates = collect_key_candidates(&operations, &dictionary);
        let violation_rate = env::var("ATROPOS_SCHEMA_VIOLATION_RATE")
            .ok()
            .and_then(|text| text.parse::<f64>().ok())
            .filter(|rate| rate.is_finite())
            .map(|rate| rate.clamp(0.0, 1.0))
            .unwrap_or(0.1);

        Ok(Self {
            name: Cow::Borrowed("deterministic-mutator"),
            operations,
            dictionary,
            key_candidates,
            violation_rate,
        })
    }

    /// Build one independent candidate for every applicable operation, in the
    /// stable order returned by `applicable_operations`. Each candidate starts
    /// from the original testcase; a successful mutation never feeds the next
    /// operation.
    pub fn deterministic_inputs<S>(
        &mut self,
        state: &mut S,
        input: &HttpInput,
    ) -> Result<Vec<HttpInput>, Error>
    where
        S: HasRand + HasCorpus<HttpInput> + HasCurrentCorpusId,
    {
        let operations = applicable_operations(state, input)?;
        let mut candidates = Vec::with_capacity(operations.len() + 1);

        for operation in operations {
            let mut candidate = input.clone();
            let changed = apply_operation(
                state,
                &mut candidate,
                operation,
                &self.operations,
                &self.dictionary,
                &self.key_candidates,
            )?;
            if changed
                && candidate != *input
                && candidate.target_bytes().len() <= NYX_INPUT_BUFFER_SIZE
            {
                candidates.push(candidate);
            }
        }

        // Keep schema-violating inputs as an optional final candidate. The
        // ordinary structured operators above still get their turn each pass.
        if state.rand_mut().coinflip(self.violation_rate) {
            let mut candidate = input.clone();
            if inject_malformed_body(&mut candidate)
                && candidate != *input
                && candidate.target_bytes().len() <= NYX_INPUT_BUFFER_SIZE
            {
                candidates.push(candidate);
            }
        }

        Ok(candidates)
    }
}

impl Named for DeterministicMutator {
    fn name(&self) -> &Cow<'static, str> {
        &self.name
    }
}

impl<S> Mutator<HttpInput, S> for DeterministicMutator
where
    S: HasRand + HasCorpus<HttpInput> + HasCurrentCorpusId,
{
    fn mutate(&mut self, state: &mut S, input: &mut HttpInput) -> Result<MutationResult, Error> {
        if state.rand_mut().coinflip(self.violation_rate) {
            return apply_with_size_limit(
                state,
                input,
                None,
                &self.operations,
                &self.dictionary,
                &self.key_candidates,
            );
        }
        mutate_one(
            state,
            input,
            &self.operations,
            &self.dictionary,
            &self.key_candidates,
        )
    }

    fn post_exec(&mut self, _state: &mut S, _new_corpus_id: Option<CorpusId>) -> Result<(), Error> {
        Ok(())
    }
}

fn load_dictionary_from_env() -> Result<Vec<String>, String> {
    let Ok(path) = env::var("ATROPOS_MUTATION_DICT") else {
        return Ok(Vec::new());
    };
    load_dictionary_file(&path)
}

fn load_dictionary_file(path: &str) -> Result<Vec<String>, String> {
    // Preflight as UTF-8 so LibAFL's line parser cannot panic on an invalid file.
    fs::read_to_string(path)
        .map_err(|err| format!("cannot read ATROPOS_MUTATION_DICT {path}: {err}"))?;
    let parsed = Tokens::from_file(path)
        .map_err(|err| format!("cannot parse AFL++ dictionary {path}: {err}"))?;

    let mut strings = Vec::new();
    let mut skipped = 0;
    for token in parsed.tokens() {
        match std::str::from_utf8(token) {
            Ok(value) => strings.push(value.to_owned()),
            Err(_) => skipped += 1,
        }
    }
    if skipped > 0 {
        eprintln!("mutation dictionary: ignored {skipped} non-UTF-8 token(s) from {path}");
    }
    Ok(strings)
}

fn collect_key_candidates(operations: &[Operation], dictionary: &[String]) -> Vec<String> {
    let mut candidates = Vec::new();
    for key in dictionary {
        push_unique_key(&mut candidates, key);
    }
    for operation in operations {
        if let Some(body) = &operation.body {
            collect_schema_keys(body, &mut candidates);
        }
    }
    candidates
}

fn collect_schema_keys(schema: &Schema, candidates: &mut Vec<String>) {
    match schema {
        Schema::Object(fields) => {
            for (key, child) in fields {
                push_unique_key(candidates, key);
                collect_schema_keys(child, candidates);
            }
        }
        Schema::Array(item) => collect_schema_keys(item, candidates),
        Schema::Any | Schema::Enum(_) | Schema::String | Schema::Number | Schema::Bool => {}
    }
}

fn push_unique_key(candidates: &mut Vec<String>, key: &str) {
    if !candidates.iter().any(|candidate| candidate == key) {
        candidates.push(key.to_owned());
    }
}

fn applicable_operations<S>(state: &S, input: &HttpInput) -> Result<Vec<MutationOp>, Error>
where
    S: HasCorpus<HttpInput> + HasCurrentCorpusId,
{
    let mut operations = Vec::new();
    if !input.pin_route {
        operations.push(MutationOp::Path);
    }

    let paths = input.body.value_paths();
    if paths
        .iter()
        .any(|path| matches!(input.body.leaf_ref(path), Some(JsonValue::String(_))))
    {
        operations.push(MutationOp::String);
    }
    if paths
        .iter()
        .any(|path| matches!(input.body.leaf_ref(path), Some(JsonValue::Number(_))))
    {
        operations.push(MutationOp::Number);
    }
    if paths
        .iter()
        .any(|path| matches!(input.body.leaf_ref(path), Some(JsonValue::Bool(_))))
    {
        operations.push(MutationOp::Bool);
    }
    if !paths.is_empty() {
        operations.push(MutationOp::NullOrType);
    }

    let object_paths = input.body.object_paths();
    if !object_paths.is_empty() {
        operations.push(MutationOp::ObjectKeyInsert);
        operations.push(MutationOp::NestNewField);
    }
    let has_nonempty_object = object_paths.iter().any(|path| {
        input
            .body
            .leaf_ref(path)
            .is_some_and(|value| matches!(value, JsonValue::Object(fields) if !fields.is_empty()))
    });
    if has_nonempty_object {
        operations.push(MutationOp::ObjectKeyRemove);
        operations.push(MutationOp::ObjectKeyRename);
        operations.push(MutationOp::NestExistingField);
    }
    if !input.body.array_paths().is_empty() {
        operations.push(MutationOp::Array);
    }
    operations.push(MutationOp::HttpMetadata);

    let current_id = state.current_corpus_id()?;
    if let Some(current_id) = current_id {
        let has_donor = state.corpus().ids().any(|id| id != current_id);
        if has_donor {
            operations.push(MutationOp::Crossover);
        }
    }
    Ok(operations)
}

fn mutate_one<S>(
    state: &mut S,
    input: &mut HttpInput,
    openapi_operations: &[Operation],
    dictionary: &[String],
    key_candidates: &[String],
) -> Result<MutationResult, Error>
where
    S: HasRand + HasCorpus<HttpInput> + HasCurrentCorpusId,
{
    let operations = applicable_operations(state, input)?;
    if operations.is_empty() {
        return Ok(MutationResult::Skipped);
    }
    let op = operations[state.rand_mut().below_or_zero(operations.len())];
    apply_with_size_limit(
        state,
        input,
        Some(op),
        openapi_operations,
        dictionary,
        key_candidates,
    )
}

fn apply_with_size_limit<S>(
    state: &mut S,
    input: &mut HttpInput,
    operation: Option<MutationOp>,
    openapi_operations: &[Operation],
    dictionary: &[String],
    key_candidates: &[String],
) -> Result<MutationResult, Error>
where
    S: HasRand + HasCorpus<HttpInput> + HasCurrentCorpusId,
{
    let mut candidate = input.clone();
    let changed = if let Some(operation) = operation {
        apply_operation(
            state,
            &mut candidate,
            operation,
            openapi_operations,
            dictionary,
            key_candidates,
        )?
    } else {
        inject_malformed_body(&mut candidate)
    };
    if !changed || candidate == *input || candidate.target_bytes().len() > NYX_INPUT_BUFFER_SIZE {
        return Ok(MutationResult::Skipped);
    }
    *input = candidate;
    Ok(MutationResult::Mutated)
}

fn apply_operation<S>(
    state: &mut S,
    input: &mut HttpInput,
    operation: MutationOp,
    openapi_operations: &[Operation],
    dictionary: &[String],
    key_candidates: &[String],
) -> Result<bool, Error>
where
    S: HasRand + HasCorpus<HttpInput> + HasCurrentCorpusId,
{
    let changed = match operation {
        MutationOp::Path => {
            input.path.push_str("/x");
            true
        }
        MutationOp::String => mutate_body_string(state, input, openapi_operations, dictionary),
        MutationOp::Number => mutate_body_number(state, input),
        MutationOp::Bool => mutate_bool_value(state, input),
        MutationOp::NullOrType => mutate_null_or_type(state, input, dictionary),
        MutationOp::ObjectKeyInsert => insert_object_key(state, input, dictionary, key_candidates),
        MutationOp::ObjectKeyRemove => remove_object_key(state, input),
        MutationOp::ObjectKeyRename => rename_object_key(state, input, key_candidates),
        MutationOp::Array => mutate_array(state, input, dictionary),
        MutationOp::NestNewField => nest_object_as_new_field(state, input, key_candidates),
        MutationOp::NestExistingField => nest_object_into_existing_field(state, input),
        MutationOp::HttpMetadata => mutate_http_metadata(state, input, dictionary),
        MutationOp::Crossover => crossover_body_subtree(state, input)?,
    };
    Ok(changed)
}

fn mutate_body_string<S: HasRand>(
    state: &mut S,
    input: &mut HttpInput,
    operations: &[Operation],
    dictionary: &[String],
) -> bool {
    let paths = input
        .body
        .value_paths()
        .into_iter()
        .filter(|path| matches!(input.body.leaf_ref(path), Some(JsonValue::String(_))))
        .collect::<Vec<_>>();
    let Some(path) = choose_path(state, &paths) else {
        return false;
    };

    let enum_choices = (|| {
        let key = input.operation_key.as_ref()?;
        let operation = operations.iter().find(|op| &op.key == key)?;
        enum_for(operation.body.as_ref(), &path, &input.body)
    })();
    if let Some(choices) = enum_choices.filter(|choices| !choices.is_empty()) {
        if state.rand_mut().below_or_zero(4) == 0 {
            if let Some(current) = input.body.leaf_ref(&path) {
                let current = match current {
                    JsonValue::String(bytes) => bytes.as_slice(),
                    JsonValue::Number(text) => text.as_bytes(),
                    _ => return false,
                };
                let different = choices
                    .into_iter()
                    .filter(|choice| choice.as_bytes() != current)
                    .collect::<Vec<_>>();
                if !different.is_empty() {
                    let choice = different[state.rand_mut().below_or_zero(different.len())].clone();
                    if let Some(JsonValue::String(bytes)) = input.body.leaf_mut(&path) {
                        *bytes = choice.into_bytes();
                        input.body_override = None;
                        return true;
                    }
                }
            }
        }
    }

    let Some(value) = input.body.leaf_mut(&path) else {
        return false;
    };
    let changed = match value {
        JsonValue::String(bytes) => mutate_string_value(state, bytes, dictionary),
        JsonValue::Number(text) => mutate_number_value(state, text),
        _ => false,
    };
    if changed {
        input.body_override = None;
    }
    changed
}

fn enum_for(schema: Option<&Schema>, path: &[usize], body: &JsonValue) -> Option<Vec<String>> {
    let mut schema = schema?;
    let mut node = body;
    for index in path {
        match (schema, node) {
            (Schema::Object(fields), JsonValue::Object(values)) => {
                schema = &fields.get(*index)?.1;
                node = &values.get(*index)?.1;
            }
            (Schema::Array(item), JsonValue::Array(values)) => {
                schema = item;
                node = values.get(*index)?;
            }
            _ => return None,
        }
    }
    match schema {
        Schema::Enum(choices) => Some(choices.clone()),
        _ => None,
    }
}

fn mutate_string_value<S: HasRand>(
    state: &mut S,
    bytes: &mut Vec<u8>,
    dictionary: &[String],
) -> bool {
    if !dictionary.is_empty() && state.rand_mut().below_or_zero(2) == 0 {
        let token = dictionary[state.rand_mut().below_or_zero(dictionary.len())].as_bytes();
        if state.rand_mut().below_or_zero(2) == 0 {
            let offset = state.rand_mut().below_or_zero(bytes.len() + 1);
            bytes.splice(offset..offset, token.iter().copied());
        } else {
            *bytes = token.to_vec();
        }
        return true;
    }
    havoc_bytes(state, bytes)
}

fn mutate_body_number<S: HasRand>(state: &mut S, input: &mut HttpInput) -> bool {
    let paths = input
        .body
        .value_paths()
        .into_iter()
        .filter(|path| matches!(input.body.leaf_ref(path), Some(JsonValue::Number(_))))
        .collect::<Vec<_>>();
    let Some(path) = choose_path(state, &paths) else {
        return false;
    };
    let Some(JsonValue::Number(text)) = input.body.leaf_mut(&path) else {
        return false;
    };
    let changed = mutate_number_value(state, text);
    if changed {
        input.body_override = None;
    }
    changed
}

fn mutate_number_value<S: HasRand>(state: &mut S, text: &mut String) -> bool {
    if state.rand_mut().below_or_zero(3) == 0 {
        let edge = NUMBER_EDGES[state.rand_mut().below_or_zero(NUMBER_EDGES.len())];
        if text == edge {
            return false;
        }
        *text = edge.to_owned();
        return true;
    }

    let delta = NUMBER_DELTAS[state.rand_mut().below_or_zero(NUMBER_DELTAS.len())];
    let negative = state.rand_mut().coinflip(0.5);
    if let Ok(value) = text.parse::<i128>() {
        let delta = if negative { -delta } else { delta };
        if let Some(result) = value.checked_add(delta) {
            let result = result.to_string();
            if result != *text {
                *text = result;
                return true;
            }
        }
    } else if let Ok(value) = text.parse::<f64>() {
        let delta = delta as f64 / 10.0;
        let result = if negative {
            value - delta
        } else {
            value + delta
        };
        if result.is_finite() {
            let result = result.to_string();
            if result != *text {
                *text = result;
                return true;
            }
        }
    }

    let edge = NUMBER_EDGES[state.rand_mut().below_or_zero(NUMBER_EDGES.len())];
    if text == edge {
        false
    } else {
        *text = edge.to_owned();
        true
    }
}

fn mutate_bool_value<S: HasRand>(state: &mut S, input: &mut HttpInput) -> bool {
    let paths = input.body.value_paths();
    let bool_paths = paths
        .into_iter()
        .filter(|path| matches!(input.body.leaf_ref(path), Some(JsonValue::Bool(_))))
        .collect::<Vec<_>>();
    let Some(path) = choose_path(state, &bool_paths) else {
        return false;
    };
    let Some(JsonValue::Bool(value)) = input.body.leaf_mut(&path) else {
        return false;
    };
    *value = !*value;
    input.body_override = None;
    true
}

fn mutate_null_or_type<S: HasRand>(
    state: &mut S,
    input: &mut HttpInput,
    dictionary: &[String],
) -> bool {
    let paths = input.body.value_paths();
    let Some(path) = choose_path(state, &paths) else {
        return false;
    };
    let Some(current) = input.body.leaf_ref(&path) else {
        return false;
    };
    let kind = json_kind(current);
    let kinds = [
        JsonKind::Null,
        JsonKind::Bool,
        JsonKind::Number,
        JsonKind::String,
        JsonKind::Array,
        JsonKind::Object,
    ]
    .into_iter()
    .filter(|candidate| *candidate != kind)
    .collect::<Vec<_>>();
    let next_kind = kinds[state.rand_mut().below_or_zero(kinds.len())];
    let replacement = random_value_of_kind(state, next_kind, dictionary);
    if let Some(value) = input.body.leaf_mut(&path) {
        *value = replacement;
        input.body_override = None;
        true
    } else {
        false
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum JsonKind {
    Null,
    Bool,
    Number,
    String,
    Array,
    Object,
}

fn json_kind(value: &JsonValue) -> JsonKind {
    match value {
        JsonValue::Null => JsonKind::Null,
        JsonValue::Bool(_) => JsonKind::Bool,
        JsonValue::Number(_) => JsonKind::Number,
        JsonValue::String(_) => JsonKind::String,
        JsonValue::Array(_) => JsonKind::Array,
        JsonValue::Object(_) => JsonKind::Object,
    }
}

fn random_value_of_kind<S: HasRand>(
    state: &mut S,
    kind: JsonKind,
    dictionary: &[String],
) -> JsonValue {
    match kind {
        JsonKind::Null => JsonValue::Null,
        JsonKind::Bool => JsonValue::Bool(state.rand_mut().coinflip(0.5)),
        JsonKind::Number => JsonValue::Number(
            NUMBER_EDGES[state.rand_mut().below_or_zero(NUMBER_EDGES.len())].to_owned(),
        ),
        JsonKind::String => JsonValue::String(random_string_bytes(state, dictionary)),
        JsonKind::Array => JsonValue::Array(Vec::new()),
        JsonKind::Object => JsonValue::Object(Vec::new()),
    }
}

fn random_string_bytes<S: HasRand>(state: &mut S, dictionary: &[String]) -> Vec<u8> {
    if !dictionary.is_empty() && state.rand_mut().below_or_zero(2) == 0 {
        dictionary[state.rand_mut().below_or_zero(dictionary.len())]
            .as_bytes()
            .to_vec()
    } else {
        CANARIES[state.rand_mut().below_or_zero(CANARIES.len())].to_vec()
    }
}

fn insert_object_key<S: HasRand>(
    state: &mut S,
    input: &mut HttpInput,
    dictionary: &[String],
    key_candidates: &[String],
) -> bool {
    let paths = input.body.object_paths();
    let Some(path) = choose_path(state, &paths) else {
        return false;
    };
    let key = candidate_key(state, key_candidates);
    let Some(fields) = input.body.object_fields_mut(&path) else {
        return false;
    };
    if fields.iter().any(|(name, _)| name == &key) {
        return false;
    }
    fields.push((
        key,
        JsonValue::String(random_string_bytes(state, dictionary)),
    ));
    input.body_override = None;
    true
}

fn remove_object_key<S: HasRand>(state: &mut S, input: &mut HttpInput) -> bool {
    let paths = input
        .body
        .object_paths()
        .into_iter()
        .filter(|path| {
            input.body.leaf_ref(path).is_some_and(
                |value| matches!(value, JsonValue::Object(fields) if !fields.is_empty()),
            )
        })
        .collect::<Vec<_>>();
    let Some(path) = choose_path(state, &paths) else {
        return false;
    };
    let Some(fields) = input.body.object_fields_mut(&path) else {
        return false;
    };
    fields.remove(state.rand_mut().below_or_zero(fields.len()));
    input.body_override = None;
    true
}

fn rename_object_key<S: HasRand>(
    state: &mut S,
    input: &mut HttpInput,
    key_candidates: &[String],
) -> bool {
    let paths = input
        .body
        .object_paths()
        .into_iter()
        .filter(|path| {
            input.body.leaf_ref(path).is_some_and(
                |value| matches!(value, JsonValue::Object(fields) if !fields.is_empty()),
            )
        })
        .collect::<Vec<_>>();
    let Some(path) = choose_path(state, &paths) else {
        return false;
    };
    let field_index = match input.body.leaf_ref(&path) {
        Some(JsonValue::Object(fields)) => state.rand_mut().below_or_zero(fields.len()),
        _ => return false,
    };
    let key = candidate_key(state, key_candidates);
    let Some(fields) = input.body.object_fields_mut(&path) else {
        return false;
    };
    if fields
        .iter()
        .enumerate()
        .any(|(index, (name, _))| index != field_index && name == &key)
        || fields[field_index].0 == key
    {
        return false;
    }
    fields[field_index].0 = key;
    input.body_override = None;
    true
}

fn candidate_key<S: HasRand>(state: &mut S, candidates: &[String]) -> String {
    if !candidates.is_empty() {
        return candidates[state.rand_mut().below_or_zero(candidates.len())].clone();
    }
    let name = format!("k{}", state.rand_mut().below_or_zero(100));
    if state.rand_mut().below_or_zero(10) == 0 {
        format!("{name}[{}]", state.rand_mut().below_or_zero(4))
    } else {
        name
    }
}

fn mutate_array<S: HasRand>(state: &mut S, input: &mut HttpInput, dictionary: &[String]) -> bool {
    let paths = input.body.array_paths();
    let Some(path) = choose_path(state, &paths) else {
        return false;
    };
    let len = match input.body.leaf_ref(&path) {
        Some(JsonValue::Array(items)) => items.len(),
        _ => return false,
    };

    #[derive(Clone, Copy)]
    enum ArrayOp {
        Insert,
        Remove,
        Duplicate,
        Swap,
        Replace,
    }
    let mut choices = vec![ArrayOp::Insert];
    if len > 0 {
        choices.extend([ArrayOp::Remove, ArrayOp::Duplicate, ArrayOp::Replace]);
    }
    if len > 1 {
        choices.push(ArrayOp::Swap);
    }
    let operation = choices[state.rand_mut().below_or_zero(choices.len())];

    match operation {
        ArrayOp::Insert => {
            let value = if len > 0 && state.rand_mut().below_or_zero(2) == 0 {
                let index = state.rand_mut().below_or_zero(len);
                match input.body.leaf_ref(&path) {
                    Some(JsonValue::Array(items)) => items[index].clone(),
                    _ => return false,
                }
            } else {
                let kind_index = state.rand_mut().below_or_zero(6);
                random_value_of_kind(
                    state,
                    [
                        JsonKind::Null,
                        JsonKind::Bool,
                        JsonKind::Number,
                        JsonKind::String,
                        JsonKind::Object,
                        JsonKind::Array,
                    ][kind_index],
                    dictionary,
                )
            };
            let index = state.rand_mut().below_or_zero(len + 1);
            if let Some(items) = input.body.array_items_mut(&path) {
                items.insert(index, value);
            }
        }
        ArrayOp::Remove => {
            let index = state.rand_mut().below_or_zero(len);
            if let Some(items) = input.body.array_items_mut(&path) {
                items.remove(index);
            }
        }
        ArrayOp::Duplicate => {
            let index = state.rand_mut().below_or_zero(len);
            let value = match input.body.leaf_ref(&path) {
                Some(JsonValue::Array(items)) => items[index].clone(),
                _ => return false,
            };
            if let Some(items) = input.body.array_items_mut(&path) {
                items.insert(index + 1, value);
            }
        }
        ArrayOp::Swap => {
            let first = state.rand_mut().below_or_zero(len);
            let mut second = state.rand_mut().below_or_zero(len - 1);
            if second >= first {
                second += 1;
            }
            if let Some(items) = input.body.array_items_mut(&path) {
                items.swap(first, second);
            }
        }
        ArrayOp::Replace => {
            let index = state.rand_mut().below_or_zero(len);
            let replacement = if len > 1 && state.rand_mut().below_or_zero(2) == 0 {
                let mut donor = state.rand_mut().below_or_zero(len - 1);
                if donor >= index {
                    donor += 1;
                }
                match input.body.leaf_ref(&path) {
                    Some(JsonValue::Array(items)) => items[donor].clone(),
                    _ => return false,
                }
            } else {
                let kind_index = state.rand_mut().below_or_zero(6);
                random_value_of_kind(
                    state,
                    [
                        JsonKind::Null,
                        JsonKind::Bool,
                        JsonKind::Number,
                        JsonKind::String,
                        JsonKind::Object,
                        JsonKind::Array,
                    ][kind_index],
                    dictionary,
                )
            };
            if let Some(items) = input.body.array_items_mut(&path) {
                if items[index] == replacement {
                    return false;
                }
                items[index] = replacement;
            }
        }
    }
    input.body_override = None;
    true
}

fn nest_object_as_new_field<S: HasRand>(
    state: &mut S,
    input: &mut HttpInput,
    key_candidates: &[String],
) -> bool {
    let paths = input.body.object_paths();
    let Some(path) = choose_path(state, &paths) else {
        return false;
    };
    let snapshot = match input.body.leaf_ref(&path) {
        Some(JsonValue::Object(_)) => input.body.leaf_ref(&path).cloned(),
        _ => None,
    };
    let Some(snapshot) = snapshot else {
        return false;
    };
    let key = candidate_key(state, key_candidates);
    let Some(fields) = input.body.object_fields_mut(&path) else {
        return false;
    };
    if fields.iter().any(|(name, _)| name == &key) {
        return false;
    }
    fields.push((key, snapshot));
    input.body_override = None;
    true
}

fn nest_object_into_existing_field<S: HasRand>(state: &mut S, input: &mut HttpInput) -> bool {
    let paths = input
        .body
        .object_paths()
        .into_iter()
        .filter(|path| {
            input.body.leaf_ref(path).is_some_and(
                |value| matches!(value, JsonValue::Object(fields) if !fields.is_empty()),
            )
        })
        .collect::<Vec<_>>();
    let Some(path) = choose_path(state, &paths) else {
        return false;
    };
    let snapshot = match input.body.leaf_ref(&path) {
        Some(JsonValue::Object(_)) => input.body.leaf_ref(&path).cloned(),
        _ => None,
    };
    let Some(snapshot) = snapshot else {
        return false;
    };
    let field_count = match input.body.leaf_ref(&path) {
        Some(JsonValue::Object(fields)) => fields.len(),
        _ => return false,
    };
    let index = state.rand_mut().below_or_zero(field_count);
    let Some(fields) = input.body.object_fields_mut(&path) else {
        return false;
    };
    if fields[index].1 == snapshot {
        return false;
    }
    fields[index].1 = snapshot;
    input.body_override = None;
    true
}

fn mutate_http_metadata<S: HasRand>(
    state: &mut S,
    input: &mut HttpInput,
    dictionary: &[String],
) -> bool {
    match state.rand_mut().below_or_zero(3) {
        0 => mutate_pair_list(state, &mut input.query, dictionary),
        1 => mutate_pair_list(state, &mut input.headers, dictionary),
        _ => mutate_pair_list(state, &mut input.cookies, dictionary),
    }
}

fn mutate_pair_list<S: HasRand>(
    state: &mut S,
    pairs: &mut Vec<(String, Vec<u8>)>,
    dictionary: &[String],
) -> bool {
    #[derive(Clone, Copy)]
    enum PairOp {
        Add,
        Remove,
        Rename,
        Value,
    }
    let mut choices = vec![PairOp::Add];
    if !pairs.is_empty() {
        choices.extend([PairOp::Remove, PairOp::Rename, PairOp::Value]);
    }
    match choices[state.rand_mut().below_or_zero(choices.len())] {
        PairOp::Add => {
            let key = candidate_key(state, dictionary);
            if pairs.iter().any(|(name, _)| name == &key) {
                return false;
            }
            pairs.push((key, random_string_bytes(state, dictionary)));
        }
        PairOp::Remove => {
            let index = state.rand_mut().below_or_zero(pairs.len());
            pairs.remove(index);
        }
        PairOp::Rename => {
            let index = state.rand_mut().below_or_zero(pairs.len());
            let key = candidate_key(state, dictionary);
            if pairs
                .iter()
                .enumerate()
                .any(|(other, (name, _))| other != index && name == &key)
                || pairs[index].0 == key
            {
                return false;
            }
            pairs[index].0 = key;
        }
        PairOp::Value => {
            let index = state.rand_mut().below_or_zero(pairs.len());
            return mutate_string_value(state, &mut pairs[index].1, dictionary);
        }
    }
    true
}

fn crossover_body_subtree<S>(state: &mut S, input: &mut HttpInput) -> Result<bool, Error>
where
    S: HasRand + HasCorpus<HttpInput> + HasCurrentCorpusId,
{
    let Some(current_id) = state.current_corpus_id()? else {
        return Ok(false);
    };
    let donor_ids = state
        .corpus()
        .ids()
        .filter(|id| *id != current_id)
        .collect::<Vec<_>>();
    if donor_ids.is_empty() {
        return Ok(false);
    }
    let donor_id = donor_ids[state.rand_mut().below_or_zero(donor_ids.len())];
    let donor = state.corpus().cloned_input_for_id(donor_id)?;
    let source_paths = donor.body.value_paths();
    let target_paths = input.body.value_paths();
    let Some(source_path) = choose_path(state, &source_paths) else {
        return Ok(false);
    };
    let Some(target_path) = choose_path(state, &target_paths) else {
        return Ok(false);
    };
    let Some(replacement) = donor.body.leaf_ref(&source_path).cloned() else {
        return Ok(false);
    };
    let Some(target) = input.body.leaf_mut(&target_path) else {
        return Ok(false);
    };
    if *target == replacement {
        return Ok(false);
    }
    *target = replacement;
    input.body_override = None;
    Ok(true)
}

fn inject_malformed_body(input: &mut HttpInput) -> bool {
    let mut raw = input.body_bytes();
    raw.push(b'{');
    raw.extend_from_slice(b"\xff");
    input.body_override = Some(raw);
    true
}

fn havoc_bytes<S: HasRand>(state: &mut S, bytes: &mut Vec<u8>) -> bool {
    let roll = state.rand_mut().below_or_zero(10);
    if roll == 3 {
        *bytes = CANARIES[state.rand_mut().below_or_zero(CANARIES.len())].to_vec();
        return true;
    }
    if bytes.is_empty() || roll == 4 {
        bytes.push(printable(state));
        return true;
    }
    match state.rand_mut().below_or_zero(4) {
        0 => {
            let index = state.rand_mut().below_or_zero(bytes.len());
            bytes[index] = printable(state);
        }
        1 => {
            let index = state.rand_mut().below_or_zero(bytes.len());
            bytes.insert(index, printable(state));
        }
        2 => {
            let index = state.rand_mut().below_or_zero(bytes.len());
            bytes.remove(index);
        }
        _ => bytes.push(printable(state)),
    }
    true
}

fn choose_path<S: HasRand, T: Clone>(state: &mut S, paths: &[T]) -> Option<T> {
    if paths.is_empty() {
        None
    } else {
        Some(paths[state.rand_mut().below_or_zero(paths.len())].clone())
    }
}

fn printable<S: HasRand>(state: &mut S) -> u8 {
    b' ' + (state.rand_mut().below_or_zero(95) as u8)
}

#[cfg(test)]
mod tests {
    use super::*;
    use libafl::corpus::{InMemoryCorpus, Testcase};
    use libafl_bolts::rands::StdRand;

    struct TestState {
        rand: StdRand,
        corpus: InMemoryCorpus<HttpInput>,
        current_id: Option<CorpusId>,
    }

    impl TestState {
        fn new(seed: u64) -> Self {
            Self {
                rand: StdRand::with_seed(seed),
                corpus: InMemoryCorpus::new(),
                current_id: None,
            }
        }
    }

    impl HasRand for TestState {
        type Rand = StdRand;

        fn rand(&self) -> &Self::Rand {
            &self.rand
        }

        fn rand_mut(&mut self) -> &mut Self::Rand {
            &mut self.rand
        }
    }

    impl HasCorpus<HttpInput> for TestState {
        type Corpus = InMemoryCorpus<HttpInput>;

        fn corpus(&self) -> &Self::Corpus {
            &self.corpus
        }

        fn corpus_mut(&mut self) -> &mut Self::Corpus {
            &mut self.corpus
        }
    }

    impl HasCurrentCorpusId for TestState {
        fn set_corpus_id(&mut self, id: CorpusId) -> Result<(), Error> {
            self.current_id = Some(id);
            Ok(())
        }

        fn clear_corpus_id(&mut self) -> Result<(), Error> {
            self.current_id = None;
            Ok(())
        }

        fn current_corpus_id(&self) -> Result<Option<CorpusId>, Error> {
            Ok(self.current_id)
        }
    }

    fn input_with_body(body: JsonValue) -> HttpInput {
        let mut input = HttpInput::batch_seed();
        input.body = body;
        input
    }

    fn temporary_dictionary(contents: &str) -> std::path::PathBuf {
        use std::time::{SystemTime, UNIX_EPOCH};

        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "atropos-mutation-dict-{}-{stamp}.dict",
            std::process::id()
        ));
        fs::write(&path, contents).unwrap();
        path
    }

    #[test]
    fn parses_afl_dictionary_and_ignores_non_utf8_tokens() {
        let path = temporary_dictionary(
            "api = \"wp-json\\x2f\"\nquoted = \"a\\\"b\"\nbinary = \"\\xff\"\n",
        );
        let tokens = load_dictionary_file(path.to_str().unwrap()).unwrap();
        fs::remove_file(path).unwrap();

        assert!(tokens.iter().any(|token| token == "wp-json/"));
        assert!(tokens.iter().any(|token| token == "a\"b"));
        assert!(!tokens.iter().any(|token| token.contains('�')));
    }

    #[test]
    fn reports_malformed_dictionary() {
        let path = temporary_dictionary("this is not a dictionary entry\n");
        let result = load_dictionary_file(path.to_str().unwrap());
        fs::remove_file(path).unwrap();

        assert!(result.is_err());
    }

    #[test]
    fn type_specific_mutations_keep_numbers_valid_and_flip_booleans() {
        let mut state = TestState::new(9);
        let mut number = "1.5".to_string();
        assert!(mutate_number_value(&mut state, &mut number));
        assert!(serde_json::from_str::<serde_json::Number>(&number).is_ok());

        let mut input = input_with_body(JsonValue::Bool(true));
        input.body_override = Some(b"old".to_vec());
        assert!(mutate_bool_value(&mut state, &mut input));
        assert_eq!(input.body, JsonValue::Bool(false));
        assert!(input.body_override.is_none());
    }

    #[test]
    fn string_mutation_uses_dictionary_tokens() {
        let mut state = TestState::new(11);
        let token = "wp-route".to_string();
        let mut observed = false;
        for _ in 0..64 {
            let mut value = b"old".to_vec();
            mutate_string_value(&mut state, &mut value, std::slice::from_ref(&token));
            if value
                .windows(token.len())
                .any(|window| window == token.as_bytes())
            {
                observed = true;
                break;
            }
        }
        assert!(observed);
    }

    #[test]
    fn null_type_mutation_clears_body_override_and_keeps_json_serializable() {
        let mut state = TestState::new(15);
        let mut input = input_with_body(JsonValue::Null);
        input.body_override = Some(b"stale".to_vec());

        assert!(mutate_null_or_type(&mut state, &mut input, &[]));
        assert_ne!(input.body, JsonValue::Null);
        assert!(input.body_override.is_none());
        let encoded = input.body_bytes();
        assert!(serde_json::from_slice::<serde_json::Value>(&encoded).is_ok());
    }

    #[test]
    fn object_key_add_remove_rename_and_self_nesting_work() {
        let mut state = TestState::new(7);
        let mut input = input_with_body(JsonValue::Object(vec![(
            "old".to_string(),
            JsonValue::String(b"value".to_vec()),
        )]));

        assert!(insert_object_key(
            &mut state,
            &mut input,
            &[],
            &["added".to_string()]
        ));
        assert!(matches!(
            input.body.leaf_ref(&[]),
            Some(JsonValue::Object(fields)) if fields.iter().any(|(key, _)| key == "added")
        ));
        let before_remove = match &input.body {
            JsonValue::Object(fields) => fields.len(),
            _ => unreachable!(),
        };
        assert!(remove_object_key(&mut state, &mut input));
        assert!(matches!(
            input.body.leaf_ref(&[]),
            Some(JsonValue::Object(fields)) if fields.len() + 1 == before_remove
        ));

        let mut rename = input_with_body(JsonValue::Object(vec![(
            "old".to_string(),
            JsonValue::Null,
        )]));
        assert!(rename_object_key(
            &mut state,
            &mut rename,
            &["renamed".to_string()]
        ));
        assert!(matches!(
            rename.body.leaf_ref(&[]),
            Some(JsonValue::Object(fields)) if fields[0].0 == "renamed"
        ));

        let original = JsonValue::Object(vec![("child".to_string(), JsonValue::Bool(true))]);
        let mut nested = input_with_body(original.clone());
        assert!(nest_object_as_new_field(
            &mut state,
            &mut nested,
            &["copy".to_string()]
        ));
        assert!(matches!(
            nested.body.leaf_ref(&[]),
            Some(JsonValue::Object(fields)) if fields.iter().any(|(key, value)| key == "copy" && value == &original)
        ));

        let mut nested_existing = input_with_body(original.clone());
        assert!(nest_object_into_existing_field(
            &mut state,
            &mut nested_existing
        ));
        assert_eq!(nested_existing.body.leaf_ref(&[0]), Some(&original));
    }

    #[test]
    fn array_mutation_changes_the_tree() {
        let mut state = TestState::new(21);
        let original = JsonValue::Array(vec![
            JsonValue::String(b"alpha".to_vec()),
            JsonValue::Number("7".to_string()),
        ]);
        let input = input_with_body(original.clone());

        let mut changed = false;
        for _ in 0..32 {
            let mut candidate = input.clone();
            if mutate_array(&mut state, &mut candidate, &[]) && candidate.body != original {
                changed = true;
                break;
            }
        }
        assert!(changed);
    }

    #[test]
    fn metadata_mutation_adds_a_query_header_or_cookie_pair() {
        let mut state = TestState::new(13);
        let mut input = input_with_body(JsonValue::Null);
        assert!(mutate_http_metadata(
            &mut state,
            &mut input,
            &["token".to_string()]
        ));
        assert_eq!(
            input.query.len() + input.headers.len() + input.cookies.len(),
            1
        );
    }

    #[test]
    fn crossover_uses_another_testcase_and_does_not_modify_its_donor() {
        let mut state = TestState::new(23);
        let current = input_with_body(JsonValue::Object(vec![(
            "current".to_string(),
            JsonValue::String(b"current".to_vec()),
        )]));
        let donor = input_with_body(JsonValue::Array(vec![JsonValue::Number("91".to_string())]));
        let current_id = state.corpus.add(Testcase::new(current.clone())).unwrap();
        let donor_id = state.corpus.add(Testcase::new(donor.clone())).unwrap();
        state.current_id = Some(current_id);

        let mut child = current.clone();
        assert!(crossover_body_subtree(&mut state, &mut child).unwrap());
        assert_ne!(child.body, current.body);
        assert_eq!(state.corpus.cloned_input_for_id(donor_id).unwrap(), donor);
    }

    #[test]
    fn crossover_is_not_available_without_another_testcase() {
        let mut state = TestState::new(31);
        let input = input_with_body(JsonValue::Null);
        let current_id = state.corpus.add(Testcase::new(input.clone())).unwrap();
        state.current_id = Some(current_id);

        let operations = applicable_operations(&state, &input).unwrap();
        assert!(!operations
            .iter()
            .any(|operation| matches!(operation, MutationOp::Crossover)));
    }

    #[test]
    fn oversized_mutation_is_skipped_without_changing_the_original() {
        let mut state = TestState::new(37);
        let mut input = HttpInput::batch_seed();
        input.path = "x".repeat(NYX_INPUT_BUFFER_SIZE);
        let original = input.clone();

        let result = apply_with_size_limit(
            &mut state,
            &mut input,
            Some(MutationOp::Path),
            &[],
            &[],
            &[],
        )
        .unwrap();
        assert!(matches!(result, MutationResult::Skipped));
        assert_eq!(input, original);
    }
}
