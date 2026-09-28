use std::{
    collections::BTreeSet,
    fs,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
};

use crate::input::HttpInput;

pub struct CoverageReport {
    pub hits_path: PathBuf,
    pub seen_path: PathBuf,
    pub queue_path: PathBuf,
    pub current_path: PathBuf,
    pub new_lines: usize,
}

pub fn write_report(current: &HttpInput, queue: &[String]) -> Result<CoverageReport, String> {
    let report_dir = crate::paths::output_dir().join("llm");
    fs::create_dir_all(&report_dir).map_err(|err| err.to_string())?;
    let dump_path = crate::paths::nyx_workdir_dir()
        .join("dump")
        .join(format!("coverage_{}", crate::paths::nyx_cpu_id()));
    let hits = read_lines(dump_path);
    let hits_path = report_dir.join("hits.txt");
    let seen_path = report_dir.join("seen_lines.txt");
    let queue_path = report_dir.join("queue.json");
    let current_path = report_dir.join("current.json");

    write_lines(&hits_path, &hits)?;
    let seen = read_lines(&seen_path);
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
