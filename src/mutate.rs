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

const DELIMITERS: &[u8] = b"/._,:; \"'";
pub(crate) const OPENAPI_COMPLIANCE_RATE: f64 = 0.5;
const BUDGET_MIN: usize = 8;
const BUDGET_MAX: usize = 64;

pub(crate) const NUMBER_EDGES: &[&str] = &[
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

pub struct InputMutator {
    name: Cow<'static, str>,
    operations: Vec<Operation>,
    dictionary: Vec<String>,
    bug_triggers: Vec<String>,
    key_candidates: Vec<String>,
    violation_rate: f64,
}

impl InputMutator {
    pub fn new(
        operations: Vec<Operation>,
        seed_tokens: &[String],
        dictionary_paths: &[std::path::PathBuf],
        bug_trigger_paths: &[std::path::PathBuf],
    ) -> Result<Self, String> {
        let mut dictionary = load_dictionary_paths(dictionary_paths)?;
        for token in seed_tokens {
            if !token.is_empty() && !dictionary.iter().any(|existing| existing == token) {
                dictionary.push(token.clone());
            }
        }
        let bug_triggers = load_dictionary_paths(bug_trigger_paths)?;
        let key_candidates = collect_key_candidates(&operations, &dictionary);
        let violation_rate = env::var("ATROPOS_SCHEMA_VIOLATION_RATE")
            .ok()
            .and_then(|text| text.parse::<f64>().ok())
            .filter(|rate| rate.is_finite())
            .map(|rate| rate.clamp(0.0, 1.0))
            .unwrap_or(0.1);

        Ok(Self {
            name: Cow::Borrowed("mutation"),
            operations,
            dictionary,
            bug_triggers,
            key_candidates,
            violation_rate,
        })
    }

    /// Build `budget` independent candidates. Each one clones the original and
    /// mutates a single randomly chosen site.
    pub fn mutation_inputs<S>(
        &mut self,
        state: &mut S,
        input: &HttpInput,
    ) -> Result<Vec<HttpInput>, Error>
    where
        S: HasRand + HasCorpus<HttpInput> + HasCurrentCorpusId,
    {
        let has_donor = has_crossover_donor(state)?;
        let sites = mutation_sites(input, has_donor);
        let mut candidates = Vec::new();
        if !sites.is_empty() {
            let budget = mutation_budget(sites.len());
            for _ in 0..budget {
                let mut candidate = input.clone();
                let site = sites[state.rand_mut().below_or_zero(sites.len())].clone();
                let changed = self.mutate_site(state, &mut candidate, &site)?;
                if changed
                    && candidate != *input
                    && candidate.target_bytes().len() <= NYX_INPUT_BUFFER_SIZE
                {
                    candidates.push(candidate);
                }
            }
        }

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

    fn mutate_site<S>(
        &mut self,
        state: &mut S,
        input: &mut HttpInput,
        site: &Site,
    ) -> Result<bool, Error>
    where
        S: HasRand + HasCorpus<HttpInput> + HasCurrentCorpusId,
    {
        let operation = crate::openapi::match_operation(input, &self.operations);
        mutate_site(
            state,
            input,
            site,
            operation,
            &self.dictionary,
            &self.bug_triggers,
            &self.key_candidates,
        )
    }
}

impl Named for InputMutator {
    fn name(&self) -> &Cow<'static, str> {
        &self.name
    }
}

impl<S> Mutator<HttpInput, S> for InputMutator
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
        let has_donor = has_crossover_donor(state)?;
        let sites = mutation_sites(input, has_donor);
        if sites.is_empty() {
            return Ok(MutationResult::Skipped);
        }
        let site = sites[state.rand_mut().below_or_zero(sites.len())].clone();
        let mut candidate = input.clone();
        let changed = self.mutate_site(state, &mut candidate, &site)?;
        if !changed || candidate == *input || candidate.target_bytes().len() > NYX_INPUT_BUFFER_SIZE
        {
            return Ok(MutationResult::Skipped);
        }
        *input = candidate;
        Ok(MutationResult::Mutated)
    }

    fn post_exec(&mut self, _state: &mut S, _new_corpus_id: Option<CorpusId>) -> Result<(), Error> {
        Ok(())
    }
}

fn load_dictionary_paths(paths: &[std::path::PathBuf]) -> Result<Vec<String>, String> {
    let mut tokens = Vec::new();
    for path in paths {
        let path = path.to_string_lossy();
        for token in load_dictionary_file(path.as_ref())? {
            if !tokens.iter().any(|existing| existing == &token) {
                tokens.push(token);
            }
        }
    }
    Ok(tokens)
}

fn load_dictionary_file(path: &str) -> Result<Vec<String>, String> {
    // Preflight as UTF-8 so LibAFL's line parser cannot panic on an invalid file.
    fs::read_to_string(path)
        .map_err(|err| format!("cannot read mutation dictionary {path}: {err}"))?;
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
        Schema::Object { fields, .. } => {
            for field in fields {
                push_unique_key(candidates, &field.name);
                collect_schema_keys(&field.schema, candidates);
            }
        }
        Schema::Array { items, .. } => collect_schema_keys(items, candidates),
        Schema::Union { variants, .. } => {
            for variant in variants {
                collect_schema_keys(variant, candidates);
            }
        }
        Schema::Any { .. }
        | Schema::Enum { .. }
        | Schema::String { .. }
        | Schema::Number { .. }
        | Schema::Bool { .. } => {}
    }
}

fn push_unique_key(candidates: &mut Vec<String>, key: &str) {
    if !candidates.iter().any(|candidate| candidate == key) {
        candidates.push(key.to_owned());
    }
}

#[derive(Clone)]
enum Site {
    Scalar(Vec<usize>),
    Object(Vec<usize>),
    Array(Vec<usize>),
    Query,
    Header,
    Cookie,
    Path,
    Crossover,
}

pub(crate) fn mutation_budget(site_count: usize) -> usize {
    site_count.saturating_mul(2).clamp(BUDGET_MIN, BUDGET_MAX)
}

fn mutation_sites(input: &HttpInput, has_donor: bool) -> Vec<Site> {
    let mut sites = Vec::new();
    for path in input.body.value_paths() {
        if matches!(
            input.body.leaf_ref(&path),
            Some(
                JsonValue::String(_) | JsonValue::Number(_) | JsonValue::Bool(_) | JsonValue::Null
            )
        ) {
            sites.push(Site::Scalar(path));
        }
    }
    for path in input.body.object_paths() {
        sites.push(Site::Object(path));
    }
    for path in input.body.array_paths() {
        sites.push(Site::Array(path));
    }
    sites.push(Site::Query);
    sites.push(Site::Header);
    sites.push(Site::Cookie);
    if !input.pin_route {
        sites.push(Site::Path);
    }
    if has_donor {
        sites.push(Site::Crossover);
    }
    sites
}

fn has_crossover_donor<S>(state: &S) -> Result<bool, Error>
where
    S: HasCorpus<HttpInput> + HasCurrentCorpusId,
{
    let Some(current_id) = state.current_corpus_id()? else {
        return Ok(false);
    };
    Ok(state.corpus().ids().any(|id| id != current_id))
}

fn mutate_site<S>(
    state: &mut S,
    input: &mut HttpInput,
    site: &Site,
    operation: Option<&Operation>,
    dictionary: &[String],
    bug_triggers: &[String],
    key_candidates: &[String],
) -> Result<bool, Error>
where
    S: HasRand + HasCorpus<HttpInput> + HasCurrentCorpusId,
{
    let changed = match site {
        Site::Scalar(path) => {
            mutate_scalar(state, input, path, operation, dictionary, bug_triggers)
        }
        Site::Object(path) => {
            mutate_object(state, input, path, operation, dictionary, key_candidates)
        }
        Site::Array(path) => mutate_array_at(state, input, path, operation, dictionary),
        Site::Query => mutate_metadata(
            state,
            input,
            crate::openapi::ParameterLocation::Query,
            operation,
            dictionary,
            bug_triggers,
        ),
        Site::Header => mutate_metadata(
            state,
            input,
            crate::openapi::ParameterLocation::Header,
            operation,
            dictionary,
            bug_triggers,
        ),
        Site::Cookie => mutate_metadata(
            state,
            input,
            crate::openapi::ParameterLocation::Cookie,
            operation,
            dictionary,
            bug_triggers,
        ),
        Site::Path => {
            if wants_compliance(state, operation.and_then(|item| item.body.as_ref())) {
                false
            } else {
                input.path.push_str("/x");
                true
            }
        }
        Site::Crossover => crossover_body_subtree(state, input)?,
    };
    Ok(changed)
}

fn wants_compliance<S: HasRand>(state: &mut S, schema: Option<&Schema>) -> bool {
    schema.is_some() && state.rand_mut().coinflip(OPENAPI_COMPLIANCE_RATE)
}

fn body_schema<'a>(
    operation: Option<&'a Operation>,
    path: &[usize],
    body: &JsonValue,
) -> Option<&'a Schema> {
    crate::openapi::schema_at(operation?.body.as_ref()?, path, body)
}

fn mutate_scalar<S: HasRand>(
    state: &mut S,
    input: &mut HttpInput,
    path: &[usize],
    operation: Option<&Operation>,
    dictionary: &[String],
    bug_triggers: &[String],
) -> bool {
    let schema = body_schema(operation, path, &input.body);
    if wants_compliance(state, schema) {
        return mutate_scalar_compliant(
            state,
            input,
            path,
            schema.unwrap(),
            dictionary,
            bug_triggers,
        );
    }
    if state.rand_mut().below_or_zero(2) == 0 {
        return replace_json_kind(state, input, path, dictionary);
    }
    let Some(value) = input.body.leaf_mut(path) else {
        return false;
    };
    let changed = match value {
        JsonValue::String(bytes) => mutate_string_value(state, bytes, dictionary, bug_triggers),
        JsonValue::Number(text) => mutate_number_value(state, text),
        JsonValue::Bool(bit) => {
            *bit = !*bit;
            true
        }
        JsonValue::Null => false,
        _ => false,
    };
    if changed {
        input.body_override = None;
    } else if matches!(input.body.leaf_ref(path), Some(JsonValue::Null)) {
        return replace_json_kind(state, input, path, dictionary);
    }
    changed
}

fn mutate_scalar_compliant<S: HasRand>(
    state: &mut S,
    input: &mut HttpInput,
    path: &[usize],
    schema: &Schema,
    dictionary: &[String],
    bug_triggers: &[String],
) -> bool {
    match schema {
        Schema::Enum { values, style, .. } => {
            let Some(current_value) = input.body.leaf_ref(path) else {
                return false;
            };
            let current = crate::openapi::enum_current(*style, current_value);
            let choices = values
                .iter()
                .filter(|choice| current.as_ref() != Some(*choice))
                .cloned()
                .collect::<Vec<_>>();
            if choices.is_empty() {
                return false;
            }
            let choice = choices[state.rand_mut().below_or_zero(choices.len())].clone();
            let Some(slot) = input.body.leaf_mut(path) else {
                return false;
            };
            *slot = crate::openapi::enum_json(*style, &choice);
        }
        Schema::String {
            min_length,
            max_length,
            ..
        } => {
            let Some(JsonValue::String(bytes)) = input.body.leaf_mut(path) else {
                let Some(slot) = input.body.leaf_mut(path) else {
                    return false;
                };
                *slot = JsonValue::String(bounded_string(state, *min_length, *max_length));
                input.body_override = None;
                return true;
            };
            let saved = bytes.clone();
            if !mutate_string_value(state, bytes, dictionary, bug_triggers)
                || !length_ok(*min_length, *max_length, bytes.len())
            {
                *bytes = saved;
                return false;
            }
        }
        Schema::Number {
            integer,
            minimum,
            maximum,
            ..
        } => {
            let Some(JsonValue::Number(text)) = input.body.leaf_mut(path) else {
                let Some(slot) = input.body.leaf_mut(path) else {
                    return false;
                };
                *slot = crate::openapi::sample_value(schema);
                input.body_override = None;
                return true;
            };
            let saved = text.clone();
            mutate_number_value(state, text);
            if !crate::openapi::number_in_range(*minimum, *maximum, *integer, text) {
                let choices =
                    crate::openapi::boundary_numbers(*integer, *minimum, *maximum, &saved);
                if choices.is_empty() {
                    *text = saved;
                    return false;
                }
                *text = choices[state.rand_mut().below_or_zero(choices.len())].clone();
            }
        }
        Schema::Bool { .. } => {
            let Some(JsonValue::Bool(bit)) = input.body.leaf_mut(path) else {
                let Some(slot) = input.body.leaf_mut(path) else {
                    return false;
                };
                *slot = JsonValue::Bool(false);
                input.body_override = None;
                return true;
            };
            *bit = !*bit;
        }
        _ => {
            let replacement = crate::openapi::sample_value(schema);
            let Some(slot) = input.body.leaf_mut(path) else {
                return false;
            };
            if *slot == replacement {
                return false;
            }
            *slot = replacement;
        }
    }
    input.body_override = None;
    true
}

fn length_ok(min_length: Option<usize>, max_length: Option<usize>, len: usize) -> bool {
    min_length.is_none_or(|min_length| len >= min_length)
        && max_length.is_none_or(|max_length| len <= max_length)
}

fn bounded_string<S: HasRand>(
    state: &mut S,
    min_length: Option<usize>,
    max_length: Option<usize>,
) -> Vec<u8> {
    let min_length = min_length.unwrap_or(0);
    let max_length = max_length.unwrap_or(min_length.max(8)).max(min_length);
    let len = if max_length == min_length {
        min_length
    } else {
        min_length + state.rand_mut().below_or_zero(max_length - min_length + 1)
    };
    printable_run(state, len.max(1))
}

fn replace_json_kind<S: HasRand>(
    state: &mut S,
    input: &mut HttpInput,
    path: &[usize],
    dictionary: &[String],
) -> bool {
    let Some(current) = input.body.leaf_ref(path) else {
        return false;
    };
    let replacement = random_different_json(state, current, dictionary);
    if let Some(value) = input.body.leaf_mut(path) {
        *value = replacement;
        input.body_override = None;
        true
    } else {
        false
    }
}

fn mutate_object<S: HasRand>(
    state: &mut S,
    input: &mut HttpInput,
    path: &[usize],
    operation: Option<&Operation>,
    dictionary: &[String],
    key_candidates: &[String],
) -> bool {
    let schema = body_schema(operation, path, &input.body);
    if wants_compliance(state, schema) {
        return mutate_object_compliant(state, input, path, schema.unwrap());
    }
    let nonempty = matches!(
        input.body.leaf_ref(path),
        Some(JsonValue::Object(fields)) if !fields.is_empty()
    );
    let mut choices = vec![ObjectOp::Insert, ObjectOp::NestNew];
    if nonempty {
        choices.extend([ObjectOp::Remove, ObjectOp::Rename, ObjectOp::NestExisting]);
    }
    match choices[state.rand_mut().below_or_zero(choices.len())] {
        ObjectOp::Insert => insert_key_at(state, input, path, dictionary, key_candidates),
        ObjectOp::Remove => remove_key_at(state, input, path),
        ObjectOp::Rename => rename_key_at(state, input, path, key_candidates),
        ObjectOp::NestNew => nest_new_at(state, input, path, key_candidates),
        ObjectOp::NestExisting => nest_existing_at(state, input, path),
    }
}

#[derive(Clone, Copy)]
enum ObjectOp {
    Insert,
    Remove,
    Rename,
    NestNew,
    NestExisting,
}

fn mutate_object_compliant<S: HasRand>(
    state: &mut S,
    input: &mut HttpInput,
    path: &[usize],
    schema: &Schema,
) -> bool {
    let Schema::Object { fields, .. } = schema else {
        return false;
    };
    let Some(JsonValue::Object(current)) = input.body.leaf_ref(path) else {
        return false;
    };
    let missing = fields
        .iter()
        .filter(|field| !current.iter().any(|(name, _)| name == &field.name))
        .collect::<Vec<_>>();
    let removable = current
        .iter()
        .enumerate()
        .filter(|(_, (name, _))| {
            fields
                .iter()
                .find(|field| &field.name == name)
                .is_none_or(|field| !field.required)
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    if missing.is_empty() && removable.is_empty() {
        return false;
    }
    let insert =
        !missing.is_empty() && (removable.is_empty() || state.rand_mut().below_or_zero(2) == 0);
    if insert {
        let field = &missing[state.rand_mut().below_or_zero(missing.len())];
        let name = field.name.clone();
        let value = crate::openapi::sample_value(&field.schema);
        let Some(slots) = input.body.object_fields_mut(path) else {
            return false;
        };
        if slots.iter().any(|(existing, _)| existing == &name) {
            return false;
        }
        slots.push((name, value));
    } else {
        let index = removable[state.rand_mut().below_or_zero(removable.len())];
        let Some(slots) = input.body.object_fields_mut(path) else {
            return false;
        };
        if index >= slots.len() {
            return false;
        }
        let name = slots[index].0.clone();
        if fields
            .iter()
            .any(|field| field.name == name && field.required)
        {
            return false;
        }
        slots.remove(index);
    }
    input.body_override = None;
    true
}

fn insert_key_at<S: HasRand>(
    state: &mut S,
    input: &mut HttpInput,
    path: &[usize],
    dictionary: &[String],
    key_candidates: &[String],
) -> bool {
    let key = candidate_key(state, key_candidates);
    let Some(fields) = input.body.object_fields_mut(path) else {
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

fn remove_key_at<S: HasRand>(state: &mut S, input: &mut HttpInput, path: &[usize]) -> bool {
    let Some(fields) = input.body.object_fields_mut(path) else {
        return false;
    };
    if fields.is_empty() {
        return false;
    }
    fields.remove(state.rand_mut().below_or_zero(fields.len()));
    input.body_override = None;
    true
}

fn rename_key_at<S: HasRand>(
    state: &mut S,
    input: &mut HttpInput,
    path: &[usize],
    key_candidates: &[String],
) -> bool {
    let field_index = match input.body.leaf_ref(path) {
        Some(JsonValue::Object(fields)) if !fields.is_empty() => {
            state.rand_mut().below_or_zero(fields.len())
        }
        _ => return false,
    };
    let key = candidate_key(state, key_candidates);
    let Some(fields) = input.body.object_fields_mut(path) else {
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

fn nest_new_at<S: HasRand>(
    state: &mut S,
    input: &mut HttpInput,
    path: &[usize],
    key_candidates: &[String],
) -> bool {
    let Some(JsonValue::Object(_)) = input.body.leaf_ref(path) else {
        return false;
    };
    let snapshot = input.body.leaf_ref(path).cloned().unwrap();
    let key = candidate_key(state, key_candidates);
    let Some(fields) = input.body.object_fields_mut(path) else {
        return false;
    };
    if fields.iter().any(|(name, _)| name == &key) {
        return false;
    }
    fields.push((key, snapshot));
    input.body_override = None;
    true
}

fn nest_existing_at<S: HasRand>(state: &mut S, input: &mut HttpInput, path: &[usize]) -> bool {
    let Some(JsonValue::Object(fields)) = input.body.leaf_ref(path) else {
        return false;
    };
    if fields.is_empty() {
        return false;
    }
    let snapshot = input.body.leaf_ref(path).cloned().unwrap();
    let index = state.rand_mut().below_or_zero(fields.len());
    let Some(fields) = input.body.object_fields_mut(path) else {
        return false;
    };
    if fields[index].1 == snapshot {
        return false;
    }
    fields[index].1 = snapshot;
    input.body_override = None;
    true
}

fn mutate_array_at<S: HasRand>(
    state: &mut S,
    input: &mut HttpInput,
    path: &[usize],
    operation: Option<&Operation>,
    dictionary: &[String],
) -> bool {
    let schema = body_schema(operation, path, &input.body);
    if wants_compliance(state, schema) {
        return mutate_array_compliant(state, input, path, schema.unwrap());
    }
    mutate_array_free(state, input, path, dictionary)
}

fn mutate_array_compliant<S: HasRand>(
    state: &mut S,
    input: &mut HttpInput,
    path: &[usize],
    schema: &Schema,
) -> bool {
    let Schema::Array {
        items,
        min_items,
        max_items,
        ..
    } = schema
    else {
        return false;
    };
    let len = match input.body.leaf_ref(path) {
        Some(JsonValue::Array(values)) => values.len(),
        _ => return false,
    };
    let min_items = min_items.unwrap_or(0);
    let can_remove = len > min_items;
    let can_add = max_items.is_none_or(|max_items| len < max_items);
    let mut choices = Vec::new();
    if can_remove {
        choices.push(0);
    }
    if can_add {
        choices.push(1);
        if len > 0 {
            choices.push(2);
        }
    }
    if choices.is_empty() {
        return false;
    }
    match choices[state.rand_mut().below_or_zero(choices.len())] {
        0 => {
            let index = state.rand_mut().below_or_zero(len);
            let Some(values) = input.body.array_items_mut(path) else {
                return false;
            };
            values.remove(index);
        }
        1 => {
            let value = crate::openapi::sample_value(items);
            let index = state.rand_mut().below_or_zero(len + 1);
            let Some(values) = input.body.array_items_mut(path) else {
                return false;
            };
            values.insert(index, value);
        }
        _ => {
            let index = state.rand_mut().below_or_zero(len);
            let Some(JsonValue::Array(values)) = input.body.leaf_ref(path) else {
                return false;
            };
            let copy = values[index].clone();
            let Some(values) = input.body.array_items_mut(path) else {
                return false;
            };
            values.insert(index + 1, copy);
        }
    }
    input.body_override = None;
    true
}

fn mutate_metadata<S: HasRand>(
    state: &mut S,
    input: &mut HttpInput,
    location: crate::openapi::ParameterLocation,
    operation: Option<&Operation>,
    dictionary: &[String],
    bug_triggers: &[String],
) -> bool {
    let parameters = operation
        .map(|operation| {
            operation
                .parameters
                .iter()
                .filter(|parameter| parameter.location == location)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let schema_present = !parameters.is_empty();
    let pairs = pair_list_mut(input, location);
    if schema_present && state.rand_mut().coinflip(OPENAPI_COMPLIANCE_RATE) {
        return mutate_pairs_compliant(state, pairs, &parameters);
    }
    mutate_pair_list(state, pairs, dictionary, bug_triggers)
}

fn pair_list_mut(
    input: &mut HttpInput,
    location: crate::openapi::ParameterLocation,
) -> &mut Vec<(String, Vec<u8>)> {
    match location {
        crate::openapi::ParameterLocation::Query => &mut input.query,
        crate::openapi::ParameterLocation::Header => &mut input.headers,
        crate::openapi::ParameterLocation::Cookie => &mut input.cookies,
    }
}

fn mutate_pairs_compliant<S: HasRand>(
    state: &mut S,
    pairs: &mut Vec<(String, Vec<u8>)>,
    parameters: &[&crate::openapi::Parameter],
) -> bool {
    let missing = parameters
        .iter()
        .filter(|parameter| {
            !pairs.iter().any(|(name, _)| {
                if matches!(
                    parameter.location,
                    crate::openapi::ParameterLocation::Header
                ) {
                    name.eq_ignore_ascii_case(&parameter.name)
                } else {
                    *name == parameter.name
                }
            })
        })
        .collect::<Vec<_>>();
    let removable = pairs
        .iter()
        .enumerate()
        .filter(|(_, (name, _))| {
            parameters
                .iter()
                .find(|parameter| {
                    if matches!(
                        parameter.location,
                        crate::openapi::ParameterLocation::Header
                    ) {
                        name.eq_ignore_ascii_case(&parameter.name)
                    } else {
                        *name == parameter.name
                    }
                })
                .is_none_or(|parameter| !parameter.required)
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let mut choices = Vec::new();
    if !missing.is_empty() {
        choices.push(0);
    }
    if !removable.is_empty() {
        choices.push(1);
    }
    if !pairs.is_empty() {
        choices.push(2);
    }
    if choices.is_empty() {
        return false;
    }
    match choices[state.rand_mut().below_or_zero(choices.len())] {
        0 => {
            let parameter = missing[state.rand_mut().below_or_zero(missing.len())];
            let value = parameter_bytes(&crate::openapi::sample_value(&parameter.schema));
            pairs.push((parameter.name.clone(), value));
        }
        1 => {
            let index = removable[state.rand_mut().below_or_zero(removable.len())];
            pairs.remove(index);
        }
        _ => {
            let index = state.rand_mut().below_or_zero(pairs.len());
            let name = pairs[index].0.clone();
            let parameter = parameters.iter().find(|parameter| {
                if matches!(
                    parameter.location,
                    crate::openapi::ParameterLocation::Header
                ) {
                    name.eq_ignore_ascii_case(&parameter.name)
                } else {
                    name == parameter.name
                }
            });
            let Some(parameter) = parameter else {
                return false;
            };
            pairs[index].1 = parameter_bytes(&crate::openapi::sample_value(&parameter.schema));
            if pairs[index].1.is_empty() && matches!(parameter.schema, Schema::String { .. }) {
                return false;
            }
        }
    }
    true
}

fn parameter_bytes(value: &JsonValue) -> Vec<u8> {
    match value {
        JsonValue::String(bytes) => bytes.clone(),
        JsonValue::Number(text) => text.as_bytes().to_vec(),
        JsonValue::Bool(true) => b"true".to_vec(),
        JsonValue::Bool(false) => b"false".to_vec(),
        JsonValue::Null => b"null".to_vec(),
        other => {
            let mut out = Vec::new();
            other.write_json(&mut out);
            out
        }
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
        JsonValue::String(bytes) => mutate_string_value(state, bytes, dictionary, &[]),
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
        schema = crate::openapi::peel_union(schema, node)?;
        match (schema, node) {
            (Schema::Object { fields, .. }, JsonValue::Object(values)) => {
                let (name, child) = values.get(*index)?;
                schema = &fields.iter().find(|field| &field.name == name)?.schema;
                node = child;
            }
            (Schema::Array { items, .. }, JsonValue::Array(values)) => {
                schema = items;
                node = values.get(*index)?;
            }
            _ => return None,
        }
    }
    schema = crate::openapi::peel_union(schema, node)?;
    match schema {
        Schema::Enum { values, .. } => Some(values.clone()),
        _ => None,
    }
}

pub(crate) fn mutate_string_value<S: HasRand>(
    state: &mut S,
    bytes: &mut Vec<u8>,
    dictionary: &[String],
    bug_triggers: &[String],
) -> bool {
    let mut sources = vec![StringSource::Havoc];
    if !dictionary.is_empty() {
        sources.push(StringSource::Dictionary);
    }
    if !bug_triggers.is_empty() {
        sources.push(StringSource::BugTrigger);
    }
    match sources[state.rand_mut().below_or_zero(sources.len())] {
        StringSource::Dictionary => splice_or_replace(state, bytes, dictionary),
        StringSource::BugTrigger => splice_or_replace(state, bytes, bug_triggers),
        StringSource::Havoc => havoc_bytes(state, bytes),
    }
}

#[derive(Clone, Copy)]
enum StringSource {
    Havoc,
    Dictionary,
    BugTrigger,
}

fn splice_or_replace<S: HasRand>(state: &mut S, bytes: &mut Vec<u8>, tokens: &[String]) -> bool {
    if tokens.is_empty() {
        return false;
    }
    let token = tokens[state.rand_mut().below_or_zero(tokens.len())].as_bytes();
    if token.is_empty() {
        return false;
    }
    if state.rand_mut().below_or_zero(2) == 0 {
        let offset = state.rand_mut().below_or_zero(bytes.len() + 1);
        bytes.splice(offset..offset, token.iter().copied());
    } else {
        *bytes = token.to_vec();
    }
    true
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

pub(crate) fn mutate_number_value<S: HasRand>(state: &mut S, text: &mut String) -> bool {
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

pub(crate) fn random_different_json<S: HasRand>(
    state: &mut S,
    current: &JsonValue,
    dictionary: &[String],
) -> JsonValue {
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
    let next = kinds[state.rand_mut().below_or_zero(kinds.len())];
    random_value_of_kind(state, next, dictionary)
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

pub(crate) fn random_string_bytes<S: HasRand>(state: &mut S, dictionary: &[String]) -> Vec<u8> {
    if !dictionary.is_empty() && state.rand_mut().below_or_zero(2) == 0 {
        dictionary[state.rand_mut().below_or_zero(dictionary.len())]
            .as_bytes()
            .to_vec()
    } else {
        let len = 1 + state.rand_mut().below_or_zero(8);
        printable_run(state, len)
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
    mutate_array_free(state, input, &path, dictionary)
}

fn mutate_array_free<S: HasRand>(
    state: &mut S,
    input: &mut HttpInput,
    path: &[usize],
    dictionary: &[String],
) -> bool {
    let len = match input.body.leaf_ref(path) {
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
        0 => mutate_pair_list(state, &mut input.query, dictionary, &[]),
        1 => mutate_pair_list(state, &mut input.headers, dictionary, &[]),
        _ => mutate_pair_list(state, &mut input.cookies, dictionary, &[]),
    }
}

fn mutate_pair_list<S: HasRand>(
    state: &mut S,
    pairs: &mut Vec<(String, Vec<u8>)>,
    dictionary: &[String],
    bug_triggers: &[String],
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
            return mutate_string_value(state, &mut pairs[index].1, dictionary, bug_triggers);
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

pub(crate) fn havoc_bytes<S: HasRand>(state: &mut S, bytes: &mut Vec<u8>) -> bool {
    if bytes.is_empty() {
        append_bytes(state, bytes);
        return true;
    }
    match state.rand_mut().below_or_zero(3) {
        0 => append_bytes(state, bytes),
        1 => replace_bytes(state, bytes),
        _ => remove_bytes(state, bytes),
    }
    true
}

fn append_bytes<S: HasRand>(state: &mut S, bytes: &mut Vec<u8>) {
    let len = 1 + state.rand_mut().below_or_zero(8);
    let payload = printable_run(state, len);
    if state.rand_mut().below_or_zero(4) != 0 {
        bytes.push(DELIMITERS[state.rand_mut().below_or_zero(DELIMITERS.len())]);
    }
    bytes.extend(payload);
}

fn replace_bytes<S: HasRand>(state: &mut S, bytes: &mut Vec<u8>) {
    let delimiter_positions = bytes
        .iter()
        .enumerate()
        .filter(|(_, byte)| DELIMITERS.contains(byte))
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    if !delimiter_positions.is_empty() {
        match state.rand_mut().below_or_zero(4) {
            0 | 1 => {
                if replace_token(state, bytes) {
                    return;
                }
            }
            2 => {
                replace_delimiter_span(state, bytes, &delimiter_positions);
                return;
            }
            _ => {}
        }
    }
    replace_span(state, bytes);
}

fn replace_token<S: HasRand>(state: &mut S, bytes: &mut Vec<u8>) -> bool {
    let mut tokens = Vec::new();
    let mut start = 0;
    for (index, byte) in bytes.iter().enumerate() {
        if DELIMITERS.contains(byte) {
            if start < index {
                tokens.push(start..index);
            }
            start = index + 1;
        }
    }
    if start < bytes.len() {
        tokens.push(start..bytes.len());
    }
    if tokens.is_empty() {
        return false;
    }
    let range = tokens[state.rand_mut().below_or_zero(tokens.len())].clone();
    bytes.splice(range, std::iter::once(printable(state)));
    true
}

fn replace_delimiter_span<S: HasRand>(state: &mut S, bytes: &mut Vec<u8>, positions: &[usize]) {
    let start = positions[state.rand_mut().below_or_zero(positions.len())];
    let delimiter = bytes[start];
    let end = bytes
        .iter()
        .enumerate()
        .skip(start + 1)
        .find(|(_, byte)| **byte == delimiter)
        .map(|(index, _)| index)
        .unwrap_or(bytes.len());
    let len = 1 + state.rand_mut().below_or_zero(8);
    let payload = printable_run(state, len);
    bytes.splice(start..end, payload);
}

fn replace_span<S: HasRand>(state: &mut S, bytes: &mut Vec<u8>) {
    let start = state.rand_mut().below_or_zero(bytes.len());
    let len = 1 + state.rand_mut().below_or_zero(bytes.len() - start);
    let payload_len = 1 + state.rand_mut().below_or_zero(8);
    let payload = printable_run(state, payload_len);
    bytes.splice(start..start + len, payload);
}

fn remove_bytes<S: HasRand>(state: &mut S, bytes: &mut Vec<u8>) {
    let start = state.rand_mut().below_or_zero(bytes.len());
    if state.rand_mut().below_or_zero(2) == 0 {
        bytes.truncate(start);
    } else {
        let end = start + state.rand_mut().below_or_zero(bytes.len() - start);
        bytes.drain(start..=end);
    }
}

fn printable_run<S: HasRand>(state: &mut S, len: usize) -> Vec<u8> {
    (0..len.max(1)).map(|_| printable(state)).collect()
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
    fn loads_dictionary_paths_and_dedups_tokens() {
        let first = temporary_dictionary("validation=\"normal\"\nmethod=\"POST\"\n");
        let second = temporary_dictionary("method=\"POST\"\npath=\"/wp/v2/posts\"\n");
        let tokens = load_dictionary_paths(&[first.clone(), second.clone()]).unwrap();
        fs::remove_file(&first).unwrap();
        fs::remove_file(&second).unwrap();

        assert_eq!(
            tokens,
            vec![
                "normal".to_string(),
                "POST".to_string(),
                "/wp/v2/posts".to_string(),
            ]
        );
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
    fn seed_tokens_join_the_dictionary_and_key_candidates() {
        let mutator = InputMutator::new(
            Vec::new(),
            &["title".to_string(), "seed".to_string(), "title".to_string()],
            &[],
            &[],
        )
        .unwrap();
        assert!(mutator.dictionary.iter().any(|token| token == "title"));
        assert!(mutator.dictionary.iter().any(|token| token == "seed"));
        assert_eq!(
            mutator
                .dictionary
                .iter()
                .filter(|token| token.as_str() == "title")
                .count(),
            1
        );
        assert!(mutator.key_candidates.iter().any(|token| token == "title"));
    }

    #[test]
    fn string_mutation_uses_dictionary_tokens() {
        let mut state = TestState::new(11);
        let token = "wp-route".to_string();
        let mut observed = false;
        for _ in 0..64 {
            let mut value = b"old".to_vec();
            mutate_string_value(&mut state, &mut value, std::slice::from_ref(&token), &[]);
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

    #[test]
    fn mutation_budget_scales_with_sites_and_stays_in_bounds() {
        assert_eq!(mutation_budget(0), 8);
        assert_eq!(mutation_budget(3), 8);
        assert_eq!(mutation_budget(10), 20);
        assert_eq!(mutation_budget(100), 64);
    }

    #[test]
    fn each_candidate_changes_only_one_site() {
        let mut state = TestState::new(4);
        let mut mutator = InputMutator::new(Vec::new(), &[], &[], &[]).unwrap();
        let input = input_with_body(JsonValue::String(b"hello/world".to_vec()));
        let candidates = mutator.mutation_inputs(&mut state, &input).unwrap();
        assert!(!candidates.is_empty());
        for candidate in candidates {
            let body_changed =
                candidate.body != input.body || candidate.body_override != input.body_override;
            let query_changed = candidate.query != input.query;
            let header_changed = candidate.headers != input.headers;
            let cookie_changed = candidate.cookies != input.cookies;
            let path_changed = candidate.path != input.path;
            let changes = [
                body_changed,
                query_changed,
                header_changed,
                cookie_changed,
                path_changed,
            ]
            .into_iter()
            .filter(|changed| *changed)
            .count();
            assert_eq!(changes, 1);
        }
    }

    #[test]
    fn compliant_object_mutation_keeps_required_keys() {
        let mut state = TestState::new(9);
        let schema = Schema::Object {
            nullable: false,
            fields: vec![
                crate::openapi::Field {
                    name: "keep".to_string(),
                    required: true,
                    schema: Schema::String {
                        min_length: None,
                        max_length: None,
                        nullable: false,
                    },
                },
                crate::openapi::Field {
                    name: "drop".to_string(),
                    required: false,
                    schema: Schema::String {
                        min_length: None,
                        max_length: None,
                        nullable: false,
                    },
                },
            ],
        };
        let original = JsonValue::Object(vec![
            ("keep".to_string(), JsonValue::String(b"a".to_vec())),
            ("drop".to_string(), JsonValue::String(b"b".to_vec())),
        ]);
        let mut removed_optional = false;
        for _ in 0..32 {
            let mut input = input_with_body(original.clone());
            assert!(mutate_object_compliant(
                &mut state,
                &mut input,
                &[],
                &schema
            ));
            let JsonValue::Object(fields) = &input.body else {
                panic!("object required");
            };
            assert!(fields.iter().any(|(name, _)| name == "keep"));
            if !fields.iter().any(|(name, _)| name == "drop") {
                removed_optional = true;
            }
        }
        assert!(removed_optional);
    }

    #[test]
    fn compliant_enum_replaces_the_current_value() {
        let mut state = TestState::new(3);
        let schema = Schema::Enum {
            values: vec!["a".to_string(), "b".to_string(), "c".to_string()],
            nullable: false,
            style: crate::openapi::EnumStyle::Text,
        };
        let mut input = input_with_body(JsonValue::String(b"a".to_vec()));
        assert!(mutate_scalar_compliant(
            &mut state,
            &mut input,
            &[],
            &schema,
            &[],
            &[]
        ));
        match input.body {
            JsonValue::String(bytes) => {
                assert!(bytes == b"b" || bytes == b"c");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn bug_trigger_is_inserted_or_replaces_the_string() {
        let mut state = TestState::new(12);
        let trigger = "TRIGGER".to_string();
        let mut inserted = false;
        let mut replaced = false;
        for _ in 0..80 {
            let mut value = b"old".to_vec();
            assert!(mutate_string_value(
                &mut state,
                &mut value,
                &[],
                std::slice::from_ref(&trigger)
            ));
            if value == trigger.as_bytes() {
                replaced = true;
            } else if value
                .windows(trigger.len())
                .any(|window| window == trigger.as_bytes())
            {
                inserted = true;
            }
            if inserted && replaced {
                break;
            }
        }
        assert!(inserted);
        assert!(replaced);
    }

    #[test]
    fn havoc_appends_replaces_tokens_and_removes_spans() {
        let mut state = TestState::new(18);
        let original = b"alpha/beta/gamma".to_vec();
        let mut appended = false;
        let mut token_replaced = false;
        let mut removed = false;
        for _ in 0..120 {
            let mut value = original.clone();
            havoc_bytes(&mut state, &mut value);
            if value.len() > original.len() && value.starts_with(&original[..5]) {
                appended = true;
            }
            if value.len() < original.len() {
                removed = true;
            }
            if value != original
                && value.len() <= original.len()
                && value.iter().any(|byte| DELIMITERS.contains(byte))
            {
                token_replaced = true;
            }
        }
        assert!(appended);
        assert!(token_replaced);
        assert!(removed);
    }
}
