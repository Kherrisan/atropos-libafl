use std::{borrow::Cow, fs, io, path::PathBuf};

use libafl::{
    corpus::Testcase,
    executors::ExitKind,
    feedbacks::{Feedback, StateInitializer},
    observers::Observer,
    Error, HasMetadata,
};
use libafl_bolts::Named;
use serde::{Deserialize, Serialize};

pub const VULN_MARKER: &str = "ATROPOS_VULN_TRIGGERED";
pub const PHP_CLI_LOG_NAME: &str = "php-cli.log";

/// One confirmed trigger line from the PHP CLI log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VulnTrigger {
    pub id: String,
    pub line: String,
}

/// Host copy of the guest `/tmp/php-cli.log`.
///
/// QEMU's `KAFL_DUMP_FILE` writes the guest file to `{workdir}/dump/php-cli.log`.
/// `pre_exec` removes that copy so a timed-out execution cannot reuse the previous request.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PhpCliLogObserver {
    name: Cow<'static, str>,
    path: PathBuf,
}

impl PhpCliLogObserver {
    pub fn new(path: PathBuf) -> Self {
        Self {
            name: Cow::Borrowed("php-cli-log"),
            path,
        }
    }
}

impl Named for PhpCliLogObserver {
    fn name(&self) -> &Cow<'static, str> {
        &self.name
    }
}

impl<I, S> Observer<I, S> for PhpCliLogObserver {
    fn pre_exec(&mut self, _state: &mut S, _input: &I) -> Result<(), Error> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|err| {
                Error::illegal_state(format!("failed to create {}: {err}", parent.display()))
            })?;
        }
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(Error::illegal_state(format!(
                "failed to reset {}: {err}",
                self.path.display()
            ))),
        }
    }
}

/// Lines whose first token is [`VULN_MARKER`] and whose second token is a finding id.
pub fn vuln_triggers(log: &[u8]) -> Vec<VulnTrigger> {
    let text = String::from_utf8_lossy(log);
    let mut triggers = Vec::new();
    for raw in text.split('\n') {
        let line = raw.trim();
        let Some(rest) = line.strip_prefix(VULN_MARKER) else {
            continue;
        };
        let rest = rest.trim_start();
        if rest.is_empty() {
            continue;
        }
        let id = rest.split_whitespace().next().unwrap_or("");
        if id.is_empty()
            || !id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        {
            continue;
        }
        triggers.push(VulnTrigger {
            id: id.to_string(),
            line: line.to_string(),
        });
    }
    triggers
}

fn trigger_ids(triggers: &[VulnTrigger]) -> String {
    let mut ids = Vec::new();
    for trigger in triggers {
        if !ids.iter().any(|id: &String| id == &trigger.id) {
            ids.push(trigger.id.clone());
        }
    }
    ids.join(",")
}

/// Objective feedback. A request is an objective when the PHP CLI log contains
/// `ATROPOS_VULN_TRIGGERED`.
#[derive(Debug)]
pub struct OracleLogFeedback {
    name: Cow<'static, str>,
    path: PathBuf,
    last_triggers: Vec<VulnTrigger>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct OracleLogMetadata {
    pub lines: Vec<String>,
}

libafl_bolts::impl_serdeany!(OracleLogMetadata);

impl OracleLogFeedback {
    pub fn new(path: PathBuf) -> Self {
        Self {
            name: Cow::Borrowed("oracle-log"),
            path,
            last_triggers: Vec::new(),
        }
    }

    fn read_log(&self) -> Result<Option<Vec<u8>>, Error> {
        match fs::read(&self.path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(Error::illegal_state(format!(
                "failed to read {}: {err}",
                self.path.display()
            ))),
        }
    }

    fn report(&self) {
        if self.last_triggers.is_empty() {
            return;
        }
        let ids = trigger_ids(&self.last_triggers);
        eprintln!("oracle objective=true vulnerability=true triggered={ids}");
        for trigger in &self.last_triggers {
            eprintln!("oracle {line}", line = trigger.line);
        }
    }
}

impl<S> StateInitializer<S> for OracleLogFeedback {}

impl Named for OracleLogFeedback {
    fn name(&self) -> &Cow<'static, str> {
        &self.name
    }
}

impl<EM, I, OT, S> Feedback<EM, I, OT, S> for OracleLogFeedback {
    fn is_interesting(
        &mut self,
        _state: &mut S,
        _manager: &mut EM,
        _input: &I,
        _observers: &OT,
        _exit_kind: &ExitKind,
    ) -> Result<bool, Error> {
        self.last_triggers.clear();
        match self.read_log()? {
            None => Ok(false),
            Some(bytes) => {
                self.last_triggers = vuln_triggers(&bytes);
                let objective = !self.last_triggers.is_empty();
                self.report();
                Ok(objective)
            }
        }
    }

    fn append_metadata(
        &mut self,
        _state: &mut S,
        _manager: &mut EM,
        _observers: &OT,
        testcase: &mut Testcase<I>,
    ) -> Result<(), Error> {
        if self.last_triggers.is_empty() {
            return Ok(());
        }
        testcase.add_metadata(OracleLogMetadata {
            lines: self
                .last_triggers
                .iter()
                .map(|trigger| trigger.line.clone())
                .collect(),
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confirmed_marker_is_an_objective_line() {
        let log = b"\
notice: nothing\n\
ATROPOS_ORACLE blocked batch_method_enum index=0 result=schema_rejected\n\
ATROPOS_VULN_TRIGGERED batch_match_desync verdict=confirmed index=1 method=POST path=/wp/v2/posts status=201 user=1 intended=posts used=users result=handler_shifted\n\
ATROPOS_ORACLE potential batch_partial_commit result=no_rollback\n\
ATROPOS_VULN_TRIGGERED batch_menu_url verdict=confirmed index=0 method=POST path=/wp/v2/menu-items status=201 user=1 url=javascript:seed result=accepted\n";
        let triggers = vuln_triggers(log);
        assert_eq!(
            triggers
                .iter()
                .map(|trigger| trigger.id.as_str())
                .collect::<Vec<_>>(),
            vec!["batch_match_desync", "batch_menu_url"]
        );
        assert!(triggers[0].line.contains("method=POST"));
        assert!(triggers[0].line.contains("path=/wp/v2/posts"));
        assert!(triggers[0].line.contains("result=handler_shifted"));
        assert_eq!(trigger_ids(&triggers), "batch_match_desync,batch_menu_url");
    }

    #[test]
    fn blocked_and_potential_lines_are_not_triggers() {
        let log = b"\
ATROPOS_ORACLE blocked batch_nested result=allow_batch_rejected\n\
ATROPOS_ORACLE potential batch_body_id_overrides_url result=body_wins\n\
ATROPOS_VULN_TRIGGERED\n\
ATROPOS_VULN_TRIGGERED bad-id verdict=confirmed\n";
        assert!(vuln_triggers(log).is_empty());
    }

    #[test]
    fn duplicate_ids_collapse_in_the_summary() {
        let log = b"\
ATROPOS_VULN_TRIGGERED batch_match_desync verdict=confirmed index=1 result=handler_shifted\n\
ATROPOS_VULN_TRIGGERED batch_match_desync verdict=confirmed index=2 result=handler_shifted\n";
        assert_eq!(trigger_ids(&vuln_triggers(log)), "batch_match_desync");
    }
}
