use std::{
    collections::BTreeSet,
    fs,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
};

use crate::input::HttpInput;

const REPORT_DIR: &str = "/home/user/atropos-libafl/llm";
const DUMP_PATH: &str = "/dev/shm/atropos/coverage_0";

pub struct CoverageReport {
    pub hits_path: PathBuf,
    pub seen_path: PathBuf,
    pub queue_path: PathBuf,
    pub current_path: PathBuf,
    pub new_lines: usize,
}

pub fn write_report(current: &HttpInput, queue: &[String]) -> Result<CoverageReport, String> {
    fs::create_dir_all(REPORT_DIR).map_err(|err| err.to_string())?;
    let hits = read_lines(DUMP_PATH);
    let hits_path = PathBuf::from(REPORT_DIR).join("hits.txt");
    let seen_path = PathBuf::from(REPORT_DIR).join("seen_lines.txt");
    let queue_path = PathBuf::from(REPORT_DIR).join("queue.json");
    let current_path = PathBuf::from(REPORT_DIR).join("current.json");

    write_lines(&hits_path, &hits)?;
    let seen = read_lines(seen_path.to_str().unwrap_or(""));
    let mut merged: BTreeSet<String> = seen.into_iter().collect();
    let before = merged.len();
    merged.extend(hits.iter().cloned());
    let new_lines = merged.len() - before;
    write_lines(&seen_path, &merged.into_iter().collect::<Vec<_>>())?;

    let queue_body = serde_json::to_string_pretty(queue).map_err(|err| err.to_string())?;
    fs::write(&queue_path, queue_body).map_err(|err| err.to_string())?;
    let current_body = serde_json::to_string_pretty(current).map_err(|err| err.to_string())?;
    fs::write(&current_path, current_body).map_err(|err| err.to_string())?;

    Ok(CoverageReport {
        hits_path,
        seen_path,
        queue_path,
        current_path,
        new_lines,
    })
}

fn read_lines(path: impl AsRef<Path>) -> Vec<String> {
    let Ok(file) = fs::File::open(path) else {
        return Vec::new();
    };
    BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter(|line| !line.is_empty())
        .collect()
}

fn write_lines(path: &Path, lines: &[String]) -> Result<(), String> {
    let mut file = fs::File::create(path).map_err(|err| err.to_string())?;
    for line in lines {
        writeln!(file, "{line}").map_err(|err| err.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unions_seen_lines() {
        let dir = std::env::temp_dir().join(format!("atropos-cov-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        // The report writer uses fixed paths. This test only checks line merge logic.
        let mut seen = BTreeSet::new();
        seen.insert("a.php:1".to_string());
        let hits = ["a.php:1".to_string(), "a.php:2".to_string()];
        let before = seen.len();
        seen.extend(hits.iter().cloned());
        assert_eq!(seen.len() - before, 1);
        let _ = fs::remove_dir_all(&dir);
    }
}
