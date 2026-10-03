mod cli;
mod coverage_report;
mod input;
mod llm;
mod mutate;
mod openapi;
mod oracle;
mod paths;
mod stage;

use std::{borrow::Cow, env, fs, path::PathBuf};

use clap::Parser;

use input::{HttpInput, NYX_INPUT_BUFFER_SIZE};
use libafl::{
    corpus::{Corpus, OnDiskCorpus, Testcase},
    events::SimpleEventManager,
    feedback_or,
    feedbacks::{CrashFeedback, MaxMapFeedback},
    fuzzer::{Evaluator, Fuzzer},
    inputs::Input,
    monitors::SimpleMonitor,
    observers::StdMapObserver,
    schedulers::QueueScheduler,
    state::{HasCorpus, HasSolutions, StdState},
    StdFuzzer,
};
use libafl_bolts::{rands::StdRand, tuples::tuple_list};
use libafl_nyx::{executor::NyxExecutor, helper::NyxHelper, settings::NyxSettings};
use llm::{LlmAgent, LlmConfig};
use mutate::InputMutator;
use stage::MutationStage;

fn load_operations(paths: &[std::path::PathBuf]) -> Result<Vec<openapi::Operation>, String> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    let operations = openapi::load_operation_files(paths)?;
    if operations.is_empty() {
        eprintln!(
            "openapi: parsed {} file(s) but found no operations",
            paths.len()
        );
    } else {
        eprintln!(
            "openapi: {} operation(s) from {} file(s)",
            operations.len(),
            paths.len()
        );
    }
    Ok(operations)
}

fn drive<E, EM, I, S, ST, Z>(
    fuzzer: &mut Z,
    stages: &mut ST,
    executor: &mut E,
    state: &mut S,
    manager: &mut EM,
) -> Result<(), Box<dyn std::error::Error>>
where
    Z: Fuzzer<E, EM, I, S, ST>,
{
    match env::var("ATROPOS_NYX_ITERS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
    {
        Some(iterations) => {
            fuzzer.fuzz_loop_for(stages, executor, state, manager, iterations)?;
        }
        None => {
            fuzzer.fuzz_loop(stages, executor, state, manager)?;
        }
    }
    Ok(())
}

fn resume_disk_corpus<S>(state: &mut S, dir: &std::path::Path) -> Result<usize, String>
where
    S: HasCorpus<HttpInput>,
{
    let mut resumed = 0;
    let entries =
        fs::read_dir(dir).map_err(|err| format!("cannot read {}: {err}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|err| format!("cannot read {}: {err}", dir.display()))?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') || name.ends_with(".json") || name.ends_with(".metadata") {
            continue;
        }
        if !entry
            .file_type()
            .map(|kind| kind.is_file())
            .unwrap_or(false)
        {
            continue;
        }
        let input = HttpInput::from_file(entry.path())
            .map_err(|err| format!("cannot load corpus input {}: {err}", entry.path().display()))?;
        let mut testcase = Testcase::new(input);
        *testcase.filename_mut() = Some(name.into_owned());
        state
            .corpus_mut()
            .add(testcase)
            .map_err(|err| format!("cannot resume corpus input: {err}"))?;
        resumed += 1;
    }
    Ok(resumed)
}

fn corpus_has_inputs(dir: &std::path::Path) -> bool {
    let Ok(entries) = fs::read_dir(dir) else {
        return false;
    };
    entries.filter_map(Result::ok).any(|entry| {
        entry
            .file_name()
            .to_str()
            .is_some_and(|name| !name.starts_with('.') && !name.ends_with(".json"))
            && entry.file_type().is_ok_and(|kind| kind.is_file())
    })
}

fn main() {
    let args = cli::Args::parse();
    if let Err(error) = run(args) {
        eprintln!("atropos-libafl: {error}");
        std::process::exit(1);
    }
}

fn run(args: cli::Args) -> Result<(), Box<dyn std::error::Error>> {
    let share_dir = args.nyx_share.clone();
    let workdir = args.nyx_workdir.clone();
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

    let corpus_dir = args.corpus_dir.clone();
    let objectives_dir = args.objectives_dir.clone();
    let corpus_ready = corpus_has_inputs(&corpus_dir);
    let (startup_seeds, seed_tokens) = if let Some(dir) = args.seed_dir() {
        let seeds = input::load_seed_directory(&dir)?;
        let tokens = input::dictionary_tokens(&seeds);
        eprintln!(
            "seeds: {} file(s) from {} contributed {} dictionary token(s)",
            seeds.len(),
            dir.display(),
            tokens.len()
        );
        if corpus_ready {
            (Vec::new(), tokens)
        } else {
            (seeds, tokens)
        }
    } else if corpus_ready {
        (Vec::new(), Vec::new())
    } else {
        return Err(
            "--seed-dir is required when the corpus directory is empty. Pass a directory containing one JSON seed per file."
                .to_string()
                .into(),
        );
    };

    let cpu_id = paths::nyx_cpu_id();
    let timeout_secs = args.timeout_secs;
    let settings = NyxSettings::builder()
        .cpu_id(cpu_id)
        .parent_cpu_id(None)
        .input_buffer_size(NYX_INPUT_BUFFER_SIZE)
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

    let dump_dir = workdir.join("dump");
    fs::create_dir_all(&corpus_dir)?;
    fs::create_dir_all(&objectives_dir)?;
    fs::create_dir_all(&dump_dir)?;
    let php_cli_log = dump_dir.join(oracle::PHP_CLI_LOG_NAME);
    let log_observer = oracle::PhpCliLogObserver::new(php_cli_log.clone());

    let mut feedback = MaxMapFeedback::new(&observer);
    // Confirmed batch oracles arrive as ATROPOS_VULN_TRIGGERED lines in the PHP CLI log.
    // PHP crashes and the older canary oracles still use Nyx's extended-crash hypercall.
    let mut objective = feedback_or!(
        oracle::OracleLogFeedback::new(php_cli_log),
        CrashFeedback::new()
    );
    let mut state = StdState::new(
        StdRand::new(),
        OnDiskCorpus::new(&corpus_dir)?,
        OnDiskCorpus::new(&objectives_dir)?,
        &mut feedback,
        &mut objective,
    )?;

    let monitor = SimpleMonitor::new(|line| println!("{line}"));
    let mut manager = SimpleEventManager::new(monitor);
    let scheduler = QueueScheduler::new();
    let mut fuzzer = StdFuzzer::new(scheduler, feedback, objective);
    let mut executor = NyxExecutor::builder().build(helper, tuple_list!(observer, log_observer));

    let operations = load_operations(&args.openapi_paths())?;
    if startup_seeds.is_empty() && corpus_ready {
        let resumed = resume_disk_corpus(&mut state, &corpus_dir)?;
        eprintln!("corpus: resumed {resumed} on-disk input(s)");
    }
    for seed in startup_seeds {
        fuzzer.add_input(&mut state, &mut executor, &mut manager, seed)?;
    }

    let mutator = InputMutator::new(
        operations.clone(),
        &seed_tokens,
        &args.dictionary_paths(),
        &args.bug_trigger_paths(),
    )?;
    let mutation = MutationStage::new(
        mutator,
        LlmAgent::new(
            LlmConfig::from_env(),
            env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            workdir.clone(),
            args.openapi_paths(),
        ),
    );

    eprintln!(
        "LibAFL Nyx ready: share={}, workdir={}, cpu={}, bitmap={} bytes",
        share_dir.display(),
        workdir.display(),
        cpu_id,
        executor.helper.bitmap_size
    );

    let mut stages = tuple_list!(mutation);
    drive(
        &mut fuzzer,
        &mut stages,
        &mut executor,
        &mut state,
        &mut manager,
    )?;

    println!(
        "corpus={} solutions={}",
        state.corpus().count(),
        state.solutions().count()
    );
    Ok(())
}
