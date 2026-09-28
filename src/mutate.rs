use std::borrow::Cow;

use libafl::{
    corpus::CorpusId,
    mutators::{MutationResult, Mutator},
    state::HasRand,
    Error,
};
use libafl_bolts::{rands::Rand, Named};

use crate::{
    input::{HttpInput, JsonValue},
    openapi::{Operation, Schema},
    redqueen::RedqueenDict,
};

const CANARIES: &[&[u8]] = &[
    b"' crash ",
    b"\" crash ",
    b"crash",
    b"crash.php",
    b"secret4815162342",
];

pub struct AtroposMutator {
    name: Cow<'static, str>,
    operations: Vec<Operation>,
    redqueen: RedqueenDict,
    violation_rate: f64,
}

impl AtroposMutator {
    pub fn new(operations: Vec<Operation>) -> Self {
        let violation_rate = std::env::var("ATROPOS_SCHEMA_VIOLATION_RATE")
            .ok()
            .and_then(|text| text.parse().ok())
            .unwrap_or(0.1);
        Self {
            name: Cow::Borrowed("atropos"),
            operations,
            redqueen: RedqueenDict::default(),
            violation_rate,
        }
    }

    pub fn reload_redqueen(&mut self) {
        let path = crate::paths::nyx_workdir_dir()
            .join("dump")
            .join(format!("strings_{}", crate::paths::nyx_cpu_id()));
        self.redqueen = RedqueenDict::load(&path);
    }
}

impl Named for AtroposMutator {
    fn name(&self) -> &Cow<'static, str> {
        &self.name
    }
}

impl<S> Mutator<HttpInput, S> for AtroposMutator
where
    S: HasRand,
{
    fn mutate(&mut self, state: &mut S, input: &mut HttpInput) -> Result<MutationResult, Error> {
        let violate = state.rand_mut().coinflip(self.violation_rate);
        if violate {
            violate_input(state, input);
        } else if !mutate_leaf(state, input, &self.operations, &self.redqueen) {
            insert_key(state, input, &self.redqueen);
        }
        Ok(MutationResult::Mutated)
    }

    fn post_exec(&mut self, _state: &mut S, _new_corpus_id: Option<CorpusId>) -> Result<(), Error> {
        self.reload_redqueen();
        Ok(())
    }
}

fn mutate_leaf<S: HasRand>(
    state: &mut S,
    input: &mut HttpInput,
    operations: &[Operation],
    redqueen: &RedqueenDict,
) -> bool {
    input.body_override = None;
    if !input.pin_route && state.rand_mut().below_or_zero(20) == 0 {
        input.path.push_str("/x");
        return true;
    }
    let paths = input.body.string_paths();
    if paths.is_empty() {
        return false;
    }
    let path = paths[state.rand_mut().below_or_zero(paths.len())].clone();
    let enum_choice = (|| {
        let key = input.operation_key.as_ref()?;
        let operation = operations.iter().find(|op| &op.key == key)?;
        let choices = enum_for(operation.body.as_ref(), &path, &input.body)?;
        let JsonValue::String(current) = input.body.leaf_ref(&path)? else {
            return None;
        };
        choices
            .into_iter()
            .find(|item| item.as_bytes() != current.as_slice())
    })();
    let Some(leaf) = input.body.leaf_mut(&path) else {
        return false;
    };
    if let Some(choice) = enum_choice {
        *leaf = JsonValue::String(choice.into_bytes());
        return true;
    }
    match leaf {
        JsonValue::String(bytes) => {
            havoc_bytes(state, bytes, redqueen);
            true
        }
        JsonValue::Number(text) => {
            let mut bytes = text.as_bytes().to_vec();
            havoc_bytes(state, &mut bytes, redqueen);
            *text = String::from_utf8_lossy(&bytes).into_owned();
            true
        }
        JsonValue::Bool(bit) => {
            *bit = !*bit;
            true
        }
        _ => false,
    }
}

fn enum_for(schema: Option<&Schema>, _path: &[usize], _body: &JsonValue) -> Option<Vec<String>> {
    // Enum constraints are applied when the leaf's schema is an enum at the root
    // of a known field. Deeper enum lookup walks the same index path as the JSON
    // object/array shape produced from the schema.
    let mut schema = schema?;
    let mut node = _body;
    for index in _path {
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

fn havoc_bytes<S: HasRand>(state: &mut S, bytes: &mut Vec<u8>, redqueen: &RedqueenDict) {
    let roll = state.rand_mut().below_or_zero(10);
    if roll == 0 && !redqueen.unmatched.is_empty() {
        let index = state.rand_mut().below_or_zero(redqueen.unmatched.len());
        let (left, right) = redqueen.unmatched[index].clone();
        if !left.is_empty() && bytes.windows(left.len()).any(|window| window == left) {
            replace_first(bytes, &left, &right);
            return;
        }
        if !right.is_empty() && bytes.windows(right.len()).any(|window| window == right) {
            replace_first(bytes, &right, &left);
            return;
        }
    }
    if roll <= 2 && !redqueen.strings.is_empty() {
        *bytes = redqueen.strings[state.rand_mut().below_or_zero(redqueen.strings.len())].clone();
        return;
    }
    if roll == 3 {
        *bytes = CANARIES[state.rand_mut().below_or_zero(CANARIES.len())].to_vec();
        return;
    }
    if bytes.is_empty() || roll == 4 {
        bytes.push(printable(state));
        return;
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
        _ => {
            if redqueen.strings.is_empty() {
                bytes.push(printable(state));
            } else {
                let extra = redqueen.strings
                    [state.rand_mut().below_or_zero(redqueen.strings.len())]
                .clone();
                bytes.extend(extra);
            }
        }
    }
}

fn insert_key<S: HasRand>(state: &mut S, input: &mut HttpInput, redqueen: &RedqueenDict) {
    let key = if !redqueen.keys.is_empty() && state.rand_mut().below_or_zero(10) < 9 {
        String::from_utf8_lossy(&redqueen.keys[state.rand_mut().below_or_zero(redqueen.keys.len())])
            .to_string()
    } else {
        let name = format!("k{}", state.rand_mut().below_or_zero(100));
        if state.rand_mut().below_or_zero(10) == 0 {
            format!("{name}[{}]", state.rand_mut().below_or_zero(4))
        } else {
            name
        }
    };
    let key = key.replace('\0', "");
    if key.is_empty() {
        return;
    }
    let paths = input.body.object_paths();
    if paths.is_empty() {
        return;
    }
    let path = paths[state.rand_mut().below_or_zero(paths.len())].clone();
    let Some(fields) = input.body.object_fields_mut(&path) else {
        return;
    };
    if fields.iter().any(|(name, _)| name == &key) {
        return;
    }
    let value = if !redqueen.strings.is_empty() {
        JsonValue::String(
            redqueen.strings[state.rand_mut().below_or_zero(redqueen.strings.len())].clone(),
        )
    } else {
        JsonValue::String(b"1".to_vec())
    };
    fields.push((key, value));
}

fn violate_input<S: HasRand>(state: &mut S, input: &mut HttpInput) {
    match state.rand_mut().below_or_zero(3) {
        0 => {
            let paths = input.body.object_paths();
            if paths.is_empty() {
                return;
            }
            let path = paths[state.rand_mut().below_or_zero(paths.len())].clone();
            if let Some(fields) = input.body.object_fields_mut(&path) {
                if !fields.is_empty() {
                    fields.remove(state.rand_mut().below_or_zero(fields.len()));
                }
            }
        }
        1 => {
            let paths = input.body.string_paths();
            if paths.is_empty() {
                return;
            }
            let path = paths[state.rand_mut().below_or_zero(paths.len())].clone();
            if let Some(leaf) = input.body.leaf_mut(&path) {
                *leaf =
                    JsonValue::Object(vec![("x".to_string(), JsonValue::String(b"y".to_vec()))]);
            }
        }
        _ => {
            let mut raw = input.body_bytes();
            raw.push(b'{');
            raw.extend_from_slice(b"\xff");
            input.body_override = Some(raw);
        }
    }
}

fn replace_first(bytes: &mut Vec<u8>, from: &[u8], to: &[u8]) {
    if let Some(position) = bytes.windows(from.len()).position(|window| window == from) {
        bytes.splice(position..position + from.len(), to.iter().copied());
    }
}

fn printable<S: HasRand>(state: &mut S) -> u8 {
    b' ' + (state.rand_mut().below_or_zero(95) as u8)
}
