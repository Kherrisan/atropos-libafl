mod coverage_report;
mod input;
mod llm;
mod mutate;
mod openapi;
mod oracle;
mod redqueen;
mod stage;

use std::{
    env, fs,
    path::PathBuf,
    time::Duration,
    ffi::CString,
};

use libafl::{
    corpus::{Corpus, OnDiskCorpus},
    events::SimpleEventManager,
    executors::{ExitKind, InProcessForkExecutor},
    feedbacks::MaxMapFeedback,
    fuzzer::{Evaluator, Fuzzer, StdFuzzer},
    monitors::SimpleMonitor,
    observers::{HitcountsMapObserver, StdMapObserver},
    schedulers::QueueScheduler,
    state::{HasCorpus, HasSolutions, StdState},
};
use libafl_bolts::{
    rands::StdRand,
    shmem::{ShMemProvider, unix_shmem},
    tuples::tuple_list,
};
use crate::{
    input::HttpInput,
    llm::{LlmAgent, LlmConfig},
    mutate::AtroposMutator,
    openapi::load_operations,
    oracle::OracleFeedback,
    stage::AtroposStage,
};

extern "C" {
    fn atropos_boot() -> i32;
    fn atropos_execute(
        method: *const i8,
        uri: *const i8,
        query: *const i8,
        content_type: *const i8,
        body: *const u8,
        body_len: usize,
        cookie: *const i8,
        headers: *const i8,
        redqueen: i32,
        exec_limit: u32,
    ) -> i32;
}

fn c_string(text: &str) -> CString {
    CString::new(text.replace('\0', "")).unwrap_or_else(|_| CString::new("").unwrap())
}

fn mmap_bitmap() -> *mut u8 {
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/shm/atropos_bitmap")
        .expect("bitmap");
    let size = 8 * 1024 * 1024;
    let ptr = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            size,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED,
            std::os::unix::io::AsRawFd::as_raw_fd(&file),
            0,
        )
    };
    if ptr == libc::MAP_FAILED {
        panic!("mmap bitmap");
    }
    ptr.cast()
}

fn seeds() -> Vec<HttpInput> {
    let path = env::var("ATROPOS_OPENAPI")
        .unwrap_or_else(|_| "/home/user/WuppieFuzz/wordpress/openapi.yaml".to_string());
    match load_operations(&path) {
        Ok(operations) if !operations.is_empty() => operations.iter().map(openapi::Operation::to_input).collect(),
        Ok(_) => vec![HttpInput::batch_seed()],
        Err(err) => {
            eprintln!("openapi: {err}");
            vec![HttpInput::batch_seed()]
        }
    }
}

fn main() {
    for (key, value) in [
        ("NYX_REPORT_LFI", "1"),
        ("NYX_INCLUDE_ERROR_IS_LFI", "1"),
        ("NYX_REPORT_EVAL", "1"),
        ("NYX_REPORT_SQL_INJECTION", "1"),
        ("NYX_REPORT_UNSERIALIZE", "1"),
    ] {
        env::set_var(key, value);
    }
    let _ = fs::write("/tmp/bug_oracle_enabled", b"1");
    unsafe {
        if atropos_boot() != 0 {
            eprintln!("atropos_boot failed");
            std::process::exit(1);
        }
    }

    let map_ptr = mmap_bitmap();
    let observer = unsafe { StdMapObserver::from_mut_ptr("bitmap", map_ptr, 8 * 1024 * 1024) };
    let observer = HitcountsMapObserver::new(observer);
    let mut feedback = MaxMapFeedback::new(&observer);
    let mut objective = OracleFeedback::new();

    let corpus_dir = PathBuf::from("/home/user/atropos-libafl/corpus");
    let solution_dir = PathBuf::from("/home/user/atropos-libafl/solutions");
    fs::create_dir_all(&corpus_dir).unwrap();
    fs::create_dir_all(&solution_dir).unwrap();

    let mut state = StdState::new(
        StdRand::new(),
        OnDiskCorpus::new(corpus_dir).unwrap(),
        OnDiskCorpus::new(solution_dir).unwrap(),
        &mut feedback,
        &mut objective,
    )
    .unwrap();

    let mon = SimpleMonitor::new(|line| println!("{line}"));
    let mut mgr = SimpleEventManager::new(mon);
    let scheduler = QueueScheduler::new();
    let mut fuzzer = StdFuzzer::new(scheduler, feedback, objective);

    let mut harness = |input: &HttpInput| {
        let method = c_string(&input.method);
        let uri = c_string(&input.path);
        let query = c_string(&input.query_string());
        let content_type = c_string(input.content_type());
        let body = input.body_bytes();
        let cookie = c_string(&input.cookie_header());
        let mut header_lines: Vec<String> = input
            .headers
            .iter()
            .map(|(name, value)| format!("{name}: {}", String::from_utf8_lossy(value)))
            .collect();
        if input.coverage_dump {
            header_lines.push("X-Atropos-Coverage: 1".to_string());
        }
        let headers = c_string(&header_lines.join("\n"));
        let rc = unsafe {
            atropos_execute(
                method.as_ptr(),
                uri.as_ptr(),
                query.as_ptr(),
                content_type.as_ptr(),
                body.as_ptr(),
                body.len(),
                cookie.as_ptr(),
                headers.as_ptr(),
                i32::from(input.redqueen),
                input.exec_limit,
            )
        };
        if rc == 0 { ExitKind::Ok } else { ExitKind::Crash }
    };

    let mut shmem_provider = unix_shmem::UnixShMemProvider::new().unwrap();
    let iterations: u64 = env::var("ATROPOS_ITERS")
        .ok()
        .and_then(|text| text.parse().ok())
        .unwrap_or(8);
    let mut executor = InProcessForkExecutor::new(
        &mut harness,
        tuple_list!(observer),
        &mut fuzzer,
        &mut state,
        &mut mgr,
        Duration::from_secs(20),
        shmem_provider,
    )
    .expect("executor");

    let operations = {
        let path = env::var("ATROPOS_OPENAPI")
            .unwrap_or_else(|_| "/home/user/WuppieFuzz/wordpress/openapi.yaml".to_string());
        load_operations(&path).unwrap_or_default()
    };
    let mut stage = AtroposStage::new(AtroposMutator::new(operations), LlmAgent::new(LlmConfig::from_env()));

    for seed in seeds() {
        fuzzer.add_input(&mut state, &mut executor, &mut mgr, seed).expect("seed");
    }
    stage.havoc.reload_redqueen();

    fuzzer
        .fuzz_loop_for(
            &mut tuple_list!(stage),
            &mut executor,
            &mut state,
            &mut mgr,
            iterations,
        )
        .expect("fuzz loop");

    println!(
        "corpus={} solutions={}",
        state.corpus().count(),
        state.solutions().count()
    );
}
