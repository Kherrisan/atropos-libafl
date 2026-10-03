//! Temporary per-execution trace. One JSON object per line in `exec-trace.jsonl`.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::time::Instant;

use libafl::executors::ExitKind;
use libafl::observers::Observer;
use libafl::Error;
use libafl_bolts::Named;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;

use crate::input::HttpInput;

#[derive(Serialize, Deserialize, Debug)]
pub struct ExecTraceObserver {
    name: Cow<'static, str>,
    path: PathBuf,
    #[serde(skip)]
    started: Option<Instant>,
    #[serde(skip)]
    next_id: u64,
}

impl ExecTraceObserver {
    pub fn create() -> Result<Self, Error> {
        let path = std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join("exec-trace.jsonl");
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|err| {
                Error::illegal_state(format!("failed to open {}: {err}", path.display()))
            })?;
        eprintln!("exec trace: {}", path.display());
        Ok(Self {
            name: Cow::Borrowed("exec-trace"),
            path,
            started: None,
            next_id: 1,
        })
    }
}

impl Named for ExecTraceObserver {
    fn name(&self) -> &Cow<'static, str> {
        &self.name
    }
}

impl<S> Observer<HttpInput, S> for ExecTraceObserver {
    fn pre_exec(&mut self, _state: &mut S, _input: &HttpInput) -> Result<(), Error> {
        self.started = Some(Instant::now());
        Ok(())
    }

    fn post_exec(
        &mut self,
        _state: &mut S,
        input: &HttpInput,
        exit_kind: &ExitKind,
    ) -> Result<(), Error> {
        let elapsed_us = self
            .started
            .take()
            .map(|started| started.elapsed().as_micros())
            .unwrap_or(0);
        let id = self.next_id;
        self.next_id += 1;
        let line = serde_json::json!({
            "n": id,
            "us": elapsed_us,
            "exit": format!("{exit_kind:?}"),
            "request": input.plaintext_json(),
        });
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|err| {
                Error::illegal_state(format!("failed to open {}: {err}", self.path.display()))
            })?;
        writeln!(file, "{line}").map_err(|err| {
            Error::illegal_state(format!("failed to write {}: {err}", self.path.display()))
        })?;
        file.flush().map_err(|err| {
            Error::illegal_state(format!("failed to flush {}: {err}", self.path.display()))
        })?;
        Ok(())
    }
}
