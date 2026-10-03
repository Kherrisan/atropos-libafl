use std::path::{Path, PathBuf};

use clap::Parser;

use crate::paths::{
    default_corpus_dir, default_nyx_share_dir, default_nyx_workdir_dir, default_objectives_dir,
};

#[derive(Debug, Parser)]
#[command(name = "atropos-libafl")]
pub struct Args {
    /// Nyx share directory containing config.ron and default_config.ron.
    #[arg(long, default_value_os_t = default_nyx_share_dir())]
    pub nyx_share: PathBuf,

    /// QEMU-Nyx work directory for this run.
    #[arg(long, default_value_os_t = default_nyx_workdir_dir())]
    pub nyx_workdir: PathBuf,

    /// Per-input execution timeout in seconds. The maximum is 255.
    #[arg(long, default_value_t = 2)]
    pub timeout_secs: u8,

    /// Directory of seed JSON files, one seed per file.
    #[arg(long)]
    pub seed_dir: Option<PathBuf>,

    /// Directory for the fuzzing corpus.
    #[arg(long, default_value_os_t = default_corpus_dir())]
    pub corpus_dir: PathBuf,

    /// Directory for objective hits.
    #[arg(long, default_value_os_t = default_objectives_dir())]
    pub objectives_dir: PathBuf,

    /// AFL++ dictionary file. Repeat the flag or separate paths with commas.
    #[arg(long, value_name = "FILE", value_delimiter = ',')]
    pub mutation_dict: Vec<PathBuf>,

    /// AFL++ dictionary of bug-trigger strings. Repeat the flag or separate paths with commas.
    #[arg(long, value_name = "FILE", value_delimiter = ',')]
    pub bug_trigger: Vec<PathBuf>,

    /// OpenAPI 2.0, 3.0, 3.1, or 3.2 YAML or JSON file. Repeat the flag or separate paths with commas.
    #[arg(long, value_name = "FILE", value_delimiter = ',')]
    pub openapi: Vec<PathBuf>,
}

impl Args {
    pub fn seed_dir(&self) -> Option<&Path> {
        self.seed_dir
            .as_deref()
            .filter(|path| !path.as_os_str().is_empty())
    }

    pub fn dictionary_paths(&self) -> Vec<PathBuf> {
        nonempty_paths(&self.mutation_dict)
    }

    pub fn bug_trigger_paths(&self) -> Vec<PathBuf> {
        nonempty_paths(&self.bug_trigger)
    }

    pub fn openapi_paths(&self) -> Vec<PathBuf> {
        nonempty_paths(&self.openapi)
    }
}

fn nonempty_paths(paths: &[PathBuf]) -> Vec<PathBuf> {
    paths
        .iter()
        .filter_map(|path| {
            let text = path.to_string_lossy();
            let trimmed = text.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(PathBuf::from(trimmed))
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fuzzer_flags_and_splits_dictionary_paths() {
        let args = Args::try_parse_from([
            "atropos-libafl",
            "--nyx-share",
            "/tmp/share",
            "--nyx-workdir",
            "/tmp/work",
            "--timeout-secs",
            "20",
            "--seed-dir",
            "/tmp/seeds",
            "--corpus-dir",
            "/tmp/corpus",
            "--objectives-dir",
            "/tmp/objectives",
            "--mutation-dict",
            "/tmp/a.dict, /tmp/b.dict",
        ])
        .unwrap();

        assert_eq!(args.nyx_share, PathBuf::from("/tmp/share"));
        assert_eq!(args.nyx_workdir, PathBuf::from("/tmp/work"));
        assert_eq!(args.timeout_secs, 20);
        assert_eq!(args.seed_dir(), Some(Path::new("/tmp/seeds")));
        assert_eq!(args.corpus_dir, PathBuf::from("/tmp/corpus"));
        assert_eq!(args.objectives_dir, PathBuf::from("/tmp/objectives"));
        assert_eq!(
            args.dictionary_paths(),
            vec![PathBuf::from("/tmp/a.dict"), PathBuf::from("/tmp/b.dict")]
        );
        assert!(args.openapi_paths().is_empty());
    }

    #[test]
    fn corpus_and_objectives_default_to_the_working_directory() {
        let args = Args::try_parse_from(["atropos-libafl"]).unwrap();
        let cwd = std::env::current_dir().unwrap();
        assert!(args.seed_dir().is_none());
        assert_eq!(args.corpus_dir, cwd.join("corpus"));
        assert_eq!(args.objectives_dir, cwd.join("objectives"));
    }

    #[test]
    fn parses_repeated_and_comma_separated_openapi_paths() {
        let args = Args::try_parse_from([
            "atropos-libafl",
            "--openapi",
            "/tmp/a.yaml, /tmp/b.json",
            "--openapi",
            "/tmp/c.yaml",
        ])
        .unwrap();

        assert_eq!(
            args.openapi_paths(),
            vec![
                PathBuf::from("/tmp/a.yaml"),
                PathBuf::from("/tmp/b.json"),
                PathBuf::from("/tmp/c.yaml")
            ]
        );
    }

    #[test]
    fn rejects_a_timeout_above_255_seconds() {
        let result = Args::try_parse_from(["atropos-libafl", "--timeout-secs", "256"]);
        assert!(result.is_err());
    }
}
