use std::{
    fs,
    io::ErrorKind,
    path::PathBuf,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::Serialize;

use crate::input::HttpInput;

#[derive(Serialize)]
pub struct QueueInputSummary {
    pub id: usize,
    pub method: String,
    pub path: String,
    pub summary: String,
}

impl QueueInputSummary {
    pub fn new(id: usize, input: &HttpInput) -> Self {
        Self {
            id,
            method: input.method.clone(),
            path: input.path.clone(),
            summary: input.summary(),
        }
    }
}

pub struct QueueCoverageReport {
    pub report_path: PathBuf,
    pub manifest_path: PathBuf,
    pub total_cases: usize,
    pub collected_cases: usize,
    pub failed_ids: Vec<usize>,
}

pub struct QueueCoverageCollector {
    coverage_dir: PathBuf,
    llm_dir: PathBuf,
    temp_dir: PathBuf,
    xml_dir: PathBuf,
    serialized_dir: PathBuf,
    cobertura_dump: PathBuf,
    serialized_dump: PathBuf,
    saved_ids: Vec<usize>,
}

#[derive(Serialize)]
struct QueueManifest<'a> {
    total_cases: usize,
    collected_cases: usize,
    complete: bool,
    failed_ids: &'a [usize],
    testcases: &'a [QueueInputSummary],
}

impl QueueCoverageCollector {
    pub fn new() -> Result<Self, String> {
        let output_dir = crate::paths::output_dir();
        let coverage_dir = output_dir.join("coverage");
        let llm_dir = output_dir.join("llm");
        fs::create_dir_all(&coverage_dir).map_err(|err| err.to_string())?;
        fs::create_dir_all(&llm_dir).map_err(|err| err.to_string())?;

        let dump_dir = crate::paths::nyx_workdir_dir().join("dump");
        let cpu_id = crate::paths::nyx_cpu_id();
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let temp_dir = coverage_dir.join(format!(
            ".queue-scan-{}-{cpu_id}-{timestamp}",
            std::process::id()
        ));
        let xml_dir = temp_dir.join("reports");
        let serialized_dir = temp_dir.join("serialized");
        fs::create_dir_all(&xml_dir).map_err(|err| err.to_string())?;
        fs::create_dir_all(&serialized_dir).map_err(|err| err.to_string())?;

        Ok(Self {
            coverage_dir,
            llm_dir,
            temp_dir,
            xml_dir,
            serialized_dir,
            cobertura_dump: dump_dir.join(format!("coverage_cobertura_{cpu_id}")),
            serialized_dump: dump_dir.join(format!("coverage_php_{cpu_id}")),
            saved_ids: Vec::new(),
        })
    }

    /// Remove fixed Nyx dump names so a failed execution cannot reuse stale data.
    pub fn clear_guest_dumps(&self) -> Result<(), String> {
        remove_if_exists(&self.cobertura_dump)?;
        remove_if_exists(&self.serialized_dump)
    }

    /// Save this execution's fresh guest reports under a testcase-specific name.
    pub fn collect_case(&mut self, id: usize) -> Result<(), String> {
        let cobertura = read_nonempty(&self.cobertura_dump, "guest Cobertura report")?;
        let serialized = read_nonempty(&self.serialized_dump, "guest PHP_CodeCoverage data")?;
        write_atomic(
            &self.xml_dir.join(format!("case-{id}.cobertura.xml")),
            &cobertura,
        )?;
        write_atomic(
            &self.serialized_dir.join(format!("case-{id}.cov")),
            &serialized,
        )?;
        self.saved_ids.push(id);
        Ok(())
    }

    pub fn finish(
        self,
        total_cases: usize,
        testcases: &[QueueInputSummary],
        mut failed_ids: Vec<usize>,
    ) -> Result<Option<QueueCoverageReport>, String> {
        failed_ids.sort_unstable();
        failed_ids.dedup();
        let manifest_path = self.llm_dir.join("queue.json");
        let complete = self.saved_ids.len() == total_cases && failed_ids.is_empty();
        let manifest = QueueManifest {
            total_cases,
            collected_cases: self.saved_ids.len(),
            complete,
            failed_ids: &failed_ids,
            testcases,
        };
        write_atomic(
            &manifest_path,
            serde_json::to_string_pretty(&manifest)
                .map_err(|err| err.to_string())?
                .as_bytes(),
        )?;

        if self.saved_ids.is_empty() {
            return Ok(None);
        }

        let php = crate::paths::nyx_php_cli();
        let phpcov = crate::paths::phpcov_binary();
        if !php.is_file() || !phpcov.is_file() {
            return Err(format!(
                "PHP coverage tools are missing (PHP CLI: {}, phpcov: {}); rebuild the PHP runtime with scripts/build-nyx-php.sh",
                php.display(),
                phpcov.display()
            ));
        }

        let cpu_id = crate::paths::nyx_cpu_id();
        let next_data = self.coverage_dir.join(format!(".queue-{cpu_id}.next.cov"));
        let next_report = self
            .coverage_dir
            .join(format!(".queue-{cpu_id}.next.cobertura.xml"));
        remove_if_exists(&next_data)?;
        remove_if_exists(&next_report)?;

        let mut library_paths = vec![crate::paths::nyx_guest_artifact_dir().join("lib")];
        if let Some(existing) = std::env::var_os("LD_LIBRARY_PATH") {
            library_paths.extend(std::env::split_paths(&existing));
        }
        let library_path = std::env::join_paths(library_paths)
            .map_err(|err| format!("invalid LD_LIBRARY_PATH for PHP coverage tools: {err}"))?;

        let remap_script =
            crate::paths::project_dir().join("coverage-tools/remap-coverage-paths.php");
        let autoload =
            crate::paths::nyx_guest_artifact_dir().join("php-code-coverage/vendor/autoload.php");
        let remap = Command::new(&php)
            .arg("-d")
            .arg("memory_limit=2G")
            .arg(&remap_script)
            .arg(&autoload)
            .arg(&self.serialized_dir)
            .arg("/var/www/html")
            .arg(crate::paths::wordpress_root())
            .env("LD_LIBRARY_PATH", &library_path)
            .output()
            .map_err(|err| {
                format!(
                    "cannot remap guest coverage paths with {}: {err}",
                    php.display()
                )
            })?;
        if !remap.status.success() {
            let stderr = String::from_utf8_lossy(&remap.stderr);
            let stdout = String::from_utf8_lossy(&remap.stdout);
            return Err(format!(
                "PHP_CodeCoverage could not map guest source paths (exit {}): {}{}",
                remap.status,
                stdout.trim(),
                stderr.trim()
            ));
        }
        let remap_summary = String::from_utf8_lossy(&remap.stdout);
        eprint!("coverage scan: {}", remap_summary);

        let merge = Command::new(&php)
            // WordPress coverage data expands substantially while phpcov builds
            // its in-memory report tree; the PHP CLI's default 128 MiB limit is
            // too small even for a single request report.
            .arg("-d")
            .arg("memory_limit=2G")
            .arg(&phpcov)
            .arg("merge")
            .arg("--php")
            .arg(&next_data)
            .arg("--cobertura")
            .arg(&next_report)
            .arg(&self.serialized_dir)
            .env("LD_LIBRARY_PATH", &library_path)
            .output()
            .map_err(|err| format!("cannot run phpcov with {}: {err}", php.display()))?;
        if !merge.status.success() {
            let stderr = String::from_utf8_lossy(&merge.stderr);
            let stdout = String::from_utf8_lossy(&merge.stdout);
            let _ = remove_if_exists(&next_data);
            let _ = remove_if_exists(&next_report);
            return Err(format!(
                "phpcov could not merge queue coverage (exit {}): {}{}",
                merge.status,
                stdout.trim(),
                stderr.trim()
            ));
        }

        let report_path = self.coverage_dir.join("queue.cobertura.xml");
        let data_path = self.coverage_dir.join("queue.cov");
        fs::rename(&next_data, &data_path).map_err(|err| err.to_string())?;
        fs::rename(&next_report, &report_path).map_err(|err| err.to_string())?;

        Ok(Some(QueueCoverageReport {
            report_path,
            manifest_path,
            total_cases,
            collected_cases: self.saved_ids.len(),
            failed_ids,
        }))
    }
}

impl Drop for QueueCoverageCollector {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.temp_dir);
    }
}

fn read_nonempty(path: &std::path::Path, description: &str) -> Result<Vec<u8>, String> {
    let contents = fs::read(path)
        .map_err(|err| format!("cannot read {description} {}: {err}", path.display()))?;
    if contents.is_empty() {
        return Err(format!("{description} {} is empty", path.display()));
    }
    Ok(contents)
}

fn remove_if_exists(path: &std::path::Path) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
        Err(err) => Err(format!("cannot remove {}: {err}", path.display())),
    }
}

fn write_atomic(path: &std::path::Path, contents: &[u8]) -> Result<(), String> {
    let temporary = path.with_extension(format!(
        "tmp-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    fs::write(&temporary, contents).map_err(|err| err.to_string())?;
    fs::rename(&temporary, path).map_err(|err| err.to_string())
}
