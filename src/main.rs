mod coverage_report;
mod input;
mod llm;
mod mutate;
mod openapi;
mod paths;
mod redqueen;
mod stage;

use std::{borrow::Cow, env, fs};

use input::HttpInput;
use libafl::{
    corpus::{Corpus, OnDiskCorpus},
    events::SimpleEventManager,
    feedbacks::{CrashFeedback, MaxMapFeedback},
    fuzzer::{Evaluator, Fuzzer},
    monitors::SimpleMonitor,
    observers::StdMapObserver,
    schedulers::QueueScheduler,
    state::{HasCorpus, HasSolutions, StdState},
    StdFuzzer,
};
use libafl_bolts::{rands::StdRand, tuples::tuple_list};
use libafl_nyx::{executor::NyxExecutor, helper::NyxHelper, settings::NyxSettings};
use llm::{LlmAgent, LlmConfig};
use mutate::AtroposMutator;
use stage::AtroposStage;

fn load_operations() -> Vec<openapi::Operation> {
    let Some(path) = paths::openapi_path() else {
        return Vec::new();
    };
    match openapi::load_operations(&path.to_string_lossy()) {
        Ok(operations) => operations,
        Err(err) => {
            eprintln!("openapi: {err}");
            Vec::new()
        }
    }
}

fn seeds(operations: &[openapi::Operation]) -> Vec<HttpInput> {
    if operations.is_empty() {
        vec![HttpInput::batch_seed()]
    } else {
        operations
            .iter()
            .map(openapi::Operation::to_input)
            .collect()
    }
}

fn main() {
    if let Err(error) = run() {
        eprintln!("atropos-libafl: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let share_dir = paths::nyx_share_dir();
    let workdir = paths::nyx_workdir_dir();
    if !share_dir.join("config.ron").is_file() {
        return Err(format!(
            "Nyx config is missing at {}; run scripts/prepare-nyx-share.sh after building the guest image",
            share_dir.join("config.ron").display()
        )
        .into());
    }
    if !share_dir.join("default_config.ron").is_file() {
        return Err(format!(
            "Nyx default config is missing at {}; run scripts/prepare-nyx-share.sh",
            share_dir.join("default_config.ron").display()
        )
        .into());
    }
    let vm_image = paths::nyx_vm_image();
    if !vm_image.is_file() {
        return Err(format!(
            "Nyx VM image is missing at {}; run scripts/create-nyx-vm.sh",
            vm_image.display()
        )
        .into());
    }
    let presnapshot = paths::nyx_presnapshot();
    if !presnapshot.is_dir() || !fs::read_dir(&presnapshot)?.next().transpose()?.is_some() {
        return Err(format!(
            "Nyx pre-snapshot is missing or empty at {}; enable KVM Nyx and run scripts/create-nyx-vm.sh",
            presnapshot.display()
        )
        .into());
    }
    fs::create_dir_all(&workdir)?;

    let cpu_id = paths::nyx_cpu_id();
    let timeout_secs = env::var("ATROPOS_NYX_TIMEOUT_SECS")
        .ok()
        .and_then(|value| value.parse::<u8>().ok())
        .unwrap_or(2);
    let settings = NyxSettings::builder()
        .cpu_id(cpu_id)
        .parent_cpu_id(None)
        .timeout_secs(timeout_secs)
        .workdir_path(Cow::Owned(workdir.to_string_lossy().into_owned()))
        .build();
    let helper = NyxHelper::new(&share_dir, settings).map_err(|err| {
        format!(
            "failed to start LibAFL Nyx from {}: {err}",
            share_dir.display()
        )
    })?;
    let observer = unsafe {
        StdMapObserver::from_mut_ptr("nyx-pcov", helper.bitmap_buffer, helper.bitmap_size)
    };

    let output_dir = paths::output_dir();
    let corpus_dir = output_dir.join("nyx-corpus");
    let solution_dir = output_dir.join("nyx-solutions");
    fs::create_dir_all(&corpus_dir)?;
    fs::create_dir_all(&solution_dir)?;

    let mut feedback = MaxMapFeedback::new(&observer);
    // Atropos reports PHP crashes and its application-level bug oracles through Nyx's
    // extended-crash hypercall, which libafl_nyx maps to ExitKind::Crash.
    let mut objective = CrashFeedback::new();
    let mut state = StdState::new(
        StdRand::new(),
        OnDiskCorpus::new(corpus_dir)?,
        OnDiskCorpus::new(solution_dir)?,
        &mut feedback,
        &mut objective,
    )?;

    let monitor = SimpleMonitor::new(|line| println!("{line}"));
    let mut manager = SimpleEventManager::new(monitor);
    let scheduler = QueueScheduler::new();
    let mut fuzzer = StdFuzzer::new(scheduler, feedback, objective);
    let mut executor = NyxExecutor::builder().build(helper, tuple_list!(observer));

    let operations = load_operations();
    if state.corpus().count() == 0 {
        for seed in seeds(&operations) {
            fuzzer.add_input(&mut state, &mut executor, &mut manager, seed)?;
        }
    }

    let mut stage = AtroposStage::new(
        AtroposMutator::new(operations),
        LlmAgent::new(LlmConfig::from_env()),
    );
    stage.havoc.reload_redqueen();
    let mut stages = tuple_list!(stage);

    eprintln!(
        "LibAFL Nyx ready: share={}, workdir={}, cpu={}, bitmap={} bytes",
        share_dir.display(),
        workdir.display(),
        cpu_id,
        executor.helper.bitmap_size
    );

    match env::var("ATROPOS_NYX_ITERS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
    {
        Some(iterations) => {
            fuzzer.fuzz_loop_for(
                &mut stages,
                &mut executor,
                &mut state,
                &mut manager,
                iterations,
            )?;
        }
        None => {
            fuzzer.fuzz_loop(&mut stages, &mut executor, &mut state, &mut manager)?;
        }
    }

    println!(
        "corpus={} solutions={}",
        state.corpus().count(),
        state.solutions().count()
    );
    Ok(())
}
