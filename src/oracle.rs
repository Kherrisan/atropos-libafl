use std::{borrow::Cow, fs};

use libafl::{Error, executors::ExitKind, feedbacks::{Feedback, StateInitializer}};
use libafl_bolts::Named;

const SECRET: &[u8] = b"secret4815162342";

extern "C" {
    fn atropos_response_ptr() -> *const u8;
    fn atropos_response_len() -> u32;
    fn atropos_crc_before() -> u32;
    fn atropos_crc_after() -> u32;
}

pub struct OracleFeedback {
    name: Cow<'static, str>,
}

impl OracleFeedback {
    pub fn new() -> Self {
        Self {
            name: Cow::Borrowed("oracle"),
        }
    }
}

impl Named for OracleFeedback {
    fn name(&self) -> &Cow<'static, str> {
        &self.name
    }
}

impl<S> StateInitializer<S> for OracleFeedback {}

impl<EM, I, OT, S> Feedback<EM, I, OT, S> for OracleFeedback {
    fn is_interesting(
        &mut self,
        _state: &mut S,
        _manager: &mut EM,
        _input: &I,
        _observers: &OT,
        exit_kind: &ExitKind,
    ) -> Result<bool, Error> {
        if matches!(exit_kind, ExitKind::Crash) {
            eprintln!("executor crash (not an oracle)");
        }
        let mut found = Vec::new();
        if let Ok(text) = fs::read_to_string("/tmp/bug_triggered") {
            let text = text.trim().to_string();
            if !text.is_empty() {
                found.push(text);
            }
        }
        let length = unsafe { atropos_response_len() } as usize;
        if length > 0 {
            let ptr = unsafe { atropos_response_ptr() };
            if !ptr.is_null() {
                let body = unsafe { std::slice::from_raw_parts(ptr, length) };
                if body.windows(SECRET.len()).any(|window| window == SECRET) {
                    found.push("response contains secret4815162342".to_string());
                }
            }
        }
        let before = unsafe { atropos_crc_before() };
        let after = unsafe { atropos_crc_after() };
        if before != after {
            found.push(format!("crash.php crc {before:#x} -> {after:#x}"));
        }
        if found.is_empty() {
            return Ok(false);
        }
        let line = found.join(" | ");
        eprintln!("ORACLE {line}");
        let _ = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open("/home/user/atropos-libafl/oracle.log")
            .and_then(|mut file| {
                use std::io::Write;
                writeln!(file, "{line}")
            });
        Ok(true)
    }
}
