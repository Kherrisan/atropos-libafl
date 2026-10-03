use std::{env, path::PathBuf, thread, time::Duration};

use libafl::{
    corpus::{Corpus, CorpusId, HasCurrentCorpusId},
    executors::{ExitKind, HasTimeout, SetTimeout},
    fuzzer::{Evaluator, ExecutesInput},
    mutators::Mutator,
    state::{HasCorpus, HasRand},
    Error,
};
use libafl_bolts::rands::Rand;
use serde::Deserialize;

use crate::{
    coverage_report::{QueueCoverageCollector, QueueCoverageReport, QueueInputSummary},
    input::HttpInput,
    mutate::InputMutator,
};

const MAX_REQUESTS_PER_SESSION: usize = 32;

pub struct LlmConfig {
    pub provider: String,
    pub auth_file: Option<PathBuf>,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub model: Option<String>,
    pub stall: u64,
    pub probability: f64,
    pub timeout: Duration,
    pub coverage_scan_timeout: Duration,
    pub requests_per_session: usize,
}

impl LlmConfig {
    pub fn from_env() -> Self {
        let provider = env::var("ATROPOS_LLM_PROVIDER").unwrap_or_else(|_| "codex".to_string());
        let auth_file = env::var("ATROPOS_LLM_AUTH_FILE")
            .ok()
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                env::var_os("HOME").map(|home| {
                    let home = PathBuf::from(home);
                    if provider == "claude" {
                        home.join(".claude")
                    } else {
                        home.join(".codex/auth.json")
                    }
                })
            });
        Self {
            provider,
            auth_file,
            base_url: env::var("ATROPOS_LLM_BASE_URL")
                .ok()
                .filter(|value| !value.is_empty()),
            api_key: env::var("ATROPOS_LLM_API_KEY")
                .ok()
                .filter(|value| !value.is_empty()),
            model: env::var("ATROPOS_LLM_MODEL")
                .ok()
                .filter(|value| !value.is_empty()),
            stall: env::var("ATROPOS_LLM_STALL")
                .ok()
                .and_then(|text| text.parse().ok())
                .unwrap_or(50),
            probability: env::var("ATROPOS_LLM_PROB")
                .ok()
                .and_then(|text| text.parse().ok())
                .unwrap_or(0.0),
            timeout: Duration::from_secs(
                env::var("ATROPOS_LLM_TIMEOUT")
                    .ok()
                    .and_then(|text| text.parse().ok())
                    .unwrap_or(180),
            ),
            coverage_scan_timeout: Duration::from_secs(
                env::var("ATROPOS_NYX_COVERAGE_TIMEOUT_SECS")
                    .ok()
                    .and_then(|text| text.parse().ok())
                    .unwrap_or(60),
            ),
            requests_per_session: env::var("ATROPOS_LLM_REQUESTS_PER_SESSION")
                .ok()
                .and_then(|text| text.parse::<usize>().ok())
                .filter(|count| *count > 0)
                .unwrap_or(1)
                .min(MAX_REQUESTS_PER_SESSION),
        }
    }
}

pub struct LlmAgent {
    config: LlmConfig,
    output_dir: PathBuf,
    nyx_workdir: PathBuf,
    openapi_paths: Vec<PathBuf>,
    execs_since_novel: u64,
}

impl LlmAgent {
    pub fn new(
        config: LlmConfig,
        output_dir: PathBuf,
        nyx_workdir: PathBuf,
        openapi_paths: Vec<PathBuf>,
    ) -> Self {
        Self {
            config,
            output_dir,
            nyx_workdir,
            openapi_paths,
            execs_since_novel: 0,
        }
    }

    pub fn execs_since_novel(&self) -> u64 {
        self.execs_since_novel
    }

    pub fn note(&mut self, corpus_id: Option<CorpusId>) {
        if corpus_id.is_some() {
            self.execs_since_novel = 0;
        } else {
            self.execs_since_novel = self.execs_since_novel.saturating_add(1);
        }
    }

    pub fn should_fire<S: HasRand>(&self, state: &mut S) -> bool {
        self.execs_since_novel >= self.config.stall
            && state.rand_mut().coinflip(self.config.probability)
    }

    /// Trace the whole enabled corpus, ask once for fresh inputs, then evaluate each one.
    pub fn generate_and_run<E, EM, S, Z>(
        &mut self,
        fuzzer: &mut Z,
        executor: &mut E,
        state: &mut S,
        manager: &mut EM,
        havoc: &mut InputMutator,
    ) -> Result<(), Error>
    where
        E: HasTimeout + SetTimeout,
        S: HasRand + HasCorpus<HttpInput> + HasCurrentCorpusId,
        Z: ExecutesInput<E, EM, HttpInput, S> + Evaluator<E, EM, HttpInput, S>,
    {
        // An unsuccessful attempt must not trigger a full corpus replay on
        // every following stage iteration. Let ordinary havoc run again.
        self.execs_since_novel = 0;

        let (total_cases, cases, mut failed_ids) = {
            let corpus = state.corpus();
            let ids = corpus.ids().collect::<Vec<_>>();
            let total_cases = ids.len();
            let mut cases = Vec::with_capacity(total_cases);
            let mut failed_ids = Vec::new();
            for id in ids {
                match corpus.cloned_input_for_id(id) {
                    Ok(input) => cases.push((id, input)),
                    Err(err) => {
                        failed_ids.push(id.0);
                        eprintln!("coverage scan: cannot load corpus testcase {id}: {err}");
                    }
                }
            }
            (total_cases, cases, failed_ids)
        };

        if total_cases == 0 {
            eprintln!("coverage scan skipped: corpus is empty");
            return Ok(());
        }
        eprintln!("coverage scan: replaying {total_cases} enabled corpus testcase(s)");

        let testcase_summaries = cases
            .iter()
            .map(|(id, input)| QueueInputSummary::new(id.0, input))
            .collect::<Vec<_>>();
        let mut collector = match QueueCoverageCollector::new(&self.output_dir, &self.nyx_workdir) {
            Ok(collector) => collector,
            Err(err) => {
                eprintln!("coverage scan could not start: {err}");
                return Ok(());
            }
        };

        let execution_timeout = executor.timeout();
        executor.set_timeout(self.config.coverage_scan_timeout);
        for (id, mut input) in cases {
            input.coverage_dump = true;
            input.redqueen = false;
            if let Err(err) = collector.clear_guest_dumps() {
                failed_ids.push(id.0);
                eprintln!("coverage scan: cannot clear dumps for testcase {id}: {err}");
                continue;
            }
            match fuzzer.execute_input(state, executor, manager, &input) {
                Ok(ExitKind::Timeout | ExitKind::Oom) => {
                    failed_ids.push(id.0);
                    eprintln!("coverage scan: testcase {id} timed out or ran out of memory");
                }
                Ok(_exit_kind) => {
                    if let Err(err) = collector.collect_case(id.0) {
                        failed_ids.push(id.0);
                        eprintln!("coverage scan: testcase {id} produced no usable report: {err}");
                    }
                }
                Err(err) => {
                    failed_ids.push(id.0);
                    eprintln!("coverage scan: testcase {id} execution failed: {err}");
                }
            }
        }
        executor.set_timeout(execution_timeout);

        let report = match collector.finish(total_cases, &testcase_summaries, failed_ids) {
            Ok(Some(report)) => report,
            Ok(None) => {
                eprintln!("coverage scan found no testcase reports; skipping agent");
                return Ok(());
            }
            Err(err) => {
                eprintln!("coverage report generation failed; skipping agent: {err}");
                return Ok(());
            }
        };
        eprintln!(
            "coverage scan collected {}/{} testcase reports{}",
            report.collected_cases,
            report.total_cases,
            if report.failed_ids.is_empty() {
                String::new()
            } else {
                format!("; failed corpus IDs: {:?}", report.failed_ids)
            }
        );

        let mut candidates = match self.ask(&report, self.config.requests_per_session) {
            Ok(candidates) => candidates,
            Err(err) => {
                eprintln!("llm generation failed; skipping candidates: {err}");
                return Ok(());
            }
        };
        let requested = self.config.requests_per_session;
        if candidates.len() > requested {
            eprintln!(
                "llm returned {} inputs; truncating to the requested {requested}",
                candidates.len()
            );
            candidates.truncate(requested);
        }
        if candidates.len() < requested {
            eprintln!(
                "llm returned {}/{} requested inputs; evaluating the available inputs",
                candidates.len(),
                requested
            );
        }

        let generated = candidates.len();
        for (index, mut candidate) in candidates.into_iter().enumerate() {
            candidate.coverage_dump = false;
            candidate.redqueen = false;
            candidate.pin_route = false;
            eprintln!(
                "llm generated [{}/{}] {}",
                index + 1,
                generated,
                candidate.summary()
            );

            let (_, corpus_id) = fuzzer.evaluate_input(state, executor, manager, &candidate)?;
            havoc.post_exec(state, corpus_id)?;
            self.note(corpus_id);
        }
        Ok(())
    }

    fn ask(
        &self,
        report: &QueueCoverageReport,
        request_count: usize,
    ) -> Result<Vec<HttpInput>, String> {
        let mut prompt = render_prompt(report, request_count, &self.openapi_label());
        if let Some(model) = &self.config.model {
            prompt = format!("使用模型 {model}。\n{prompt}");
        }
        let output = self.run_acp(&prompt)?;
        parse_http_inputs(&output).ok_or_else(|| "agent output had no HttpInput JSON".to_string())
    }

    fn run_acp(&self, prompt: &str) -> Result<String, String> {
        let prompt = prompt.to_string();
        let agent = self.acp_agent()?;
        let timeout = self.config.timeout;
        let work = agent_client_protocol::Client
            .builder()
            .name("atropos-libafl")
            .on_receive_request(
                async move |request: agent_client_protocol::schema::v1::RequestPermissionRequest, responder, _connection| {
                    responder.respond(permission_response(&request))?;
                    Ok(())
                },
                agent_client_protocol::on_receive_request!(),
            )
            .connect_with(agent, async move |connection| {
                connection
                    .send_request(agent_client_protocol::schema::v1::InitializeRequest::new(
                        agent_client_protocol::schema::ProtocolVersion::V1,
                    ))
                    .block_task()
                    .await?;
                connection
                    .build_session(&crate::paths::wordpress_root())
                    .block_task()
                    .run_until(async |mut session| {
                        session.send_prompt(prompt)?;
                        session.read_to_string().await
                    })
                    .await
            });
        let result = futures::executor::block_on(async {
            futures::pin_mut!(work);
            let seconds = timeout.as_secs();
            let timeout_fut = sleep_timeout(timeout);
            match futures::future::select(work, timeout_fut).await {
                futures::future::Either::Left((value, _)) => value.map_err(|err| err.to_string()),
                futures::future::Either::Right((_, _)) => {
                    Err(format!("agent timed out after {seconds}s"))
                }
            }
        });
        result
    }

    fn acp_agent(&self) -> Result<agent_client_protocol::AcpAgent, String> {
        let package = if self.config.provider == "claude" {
            "@agentclientprotocol/claude-agent-acp@0.81.2"
        } else {
            "@agentclientprotocol/codex-acp@1.13.1"
        };
        let mut args = self.auth_env();
        args.extend(["npx".to_string(), "-y".to_string(), package.to_string()]);
        agent_client_protocol::AcpAgent::from_args(args).map_err(|err| err.to_string())
    }

    fn auth_env(&self) -> Vec<String> {
        let mut env_vars = Vec::new();
        if self.config.provider != "claude" {
            // codex-acp selects its initial permission/sandbox preset from this
            // variable. Keep Atropos' ACP session in the user-requested mode.
            env_vars.push("INITIAL_AGENT_MODE=agent-full-access".to_string());
        }
        if let Some(key) = &self.config.api_key {
            if self.config.provider == "claude" {
                env_vars.push(format!("ANTHROPIC_API_KEY={key}"));
            } else {
                env_vars.push(format!("OPENAI_API_KEY={key}"));
            }
        }
        if let Some(url) = &self.config.base_url {
            if self.config.provider == "claude" {
                env_vars.push(format!("ANTHROPIC_BASE_URL={url}"));
            } else {
                env_vars.push(format!("OPENAI_BASE_URL={url}"));
            }
        }
        if self.config.api_key.is_none() {
            if let Some(path) = &self.config.auth_file {
                if self.config.provider == "claude" {
                    let dir = if path.is_dir() {
                        path.clone()
                    } else {
                        path.parent().unwrap_or(path).to_path_buf()
                    };
                    env_vars.push(format!("CLAUDE_CONFIG_DIR={}", dir.display()));
                } else if let Some(parent) = path.parent() {
                    env_vars.push(format!("CODEX_HOME={}", parent.display()));
                }
            }
        }
        env_vars
    }

    fn openapi_label(&self) -> String {
        if self.openapi_paths.is_empty() {
            "未配置".to_string()
        } else {
            self.openapi_paths
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        }
    }
}

fn render_prompt(report: &QueueCoverageReport, request_count: usize, openapi: &str) -> String {
    let wordpress_root = crate::paths::wordpress_root();
    let scan_status = if report.failed_ids.is_empty()
        && report.collected_cases == report.total_cases
    {
        format!(
            "完整：成功采集 {}/{} 个 testcase。",
            report.collected_cases, report.total_cases
        )
    } else {
        format!(
            "不完整：成功采集 {}/{} 个 testcase；失败 corpus ID：{:?}。未覆盖行只代表成功采集的 testcase 没有命中，失败样本的覆盖未知。",
            report.collected_cases, report.total_cases, report.failed_ids
        )
    };
    format!(
        r#"你是覆盖引导的 HTTP 请求生成器。不要修改任何文件，不要执行写入。只在最终回复里打印一个 JSON 数组。

请阅读队列级 Cobertura 报告和完整 corpus 请求清单，检查报告中尚未覆盖的 PHP 行及其源码，然后生成 {request_count} 条全新的 HTTP 请求，目标是进入新的代码路径并产生新的覆盖。每条请求都必须是独立的 HttpInput，彼此不要重复；不要把某个现有 testcase 当作待修改的当前输入，也不要沿用任何 testcase 的 method、path 或 body；先检查请求清单以避免照抄已有请求。
本轮覆盖扫描状态：{scan_status}
主机上的 WordPress 源码根目录是 {wordpress_root}。解析 Cobertura 的 <source> 与 class filename：若 source 使用 guest 的 /var/www/html 路径，先将该前缀映射到主机源码根目录，再拼接相对 filename。完整扫描中 line hits=0 表示 corpus 未覆盖该行，hits>0 表示已覆盖；若扫描不完整，失败 testcase 的覆盖未知。入口是 index.php，REST 由 WordPress 的 wp() 分发。
Cobertura 报告可能很大，不要把整份 XML 载入上下文或原样输出。先筛选 1 到 3 个覆盖率低且与 REST 请求路径相关的文件，再只查看这些文件的未覆盖行和对应源码。
可选 OpenAPI 文档路径：{openapi}。若已配置，请用它确认 method、path 和请求结构；也可结合 WordPress 源码探索其他有效入口。

队列级 Cobertura 报告：{coverage}
完整 corpus 请求清单：{manifest}

最终只输出一个 JSON 数组，恰好包含 {request_count} 个 HttpInput 对象。每个对象的字段为 method, path, query, headers, cookies, body, body_override, operation_key, pin_route, exec_limit, redqueen, coverage_dump。不要输出说明或 Markdown。pin_route、redqueen、coverage_dump 必须为 false。query、headers、cookies 是 [字符串, 字节数组] 的列表。body 是 JSON 值的树：Null、Bool、Number 字符串、String 字节数组、Array、Object 字段列表。
"#,
        coverage = report.report_path.display(),
        manifest = report.manifest_path.display(),
        scan_status = scan_status,
        openapi = openapi,
        wordpress_root = wordpress_root.display(),
        request_count = request_count,
    )
}

fn permission_response(
    request: &agent_client_protocol::schema::v1::RequestPermissionRequest,
) -> agent_client_protocol::schema::v1::RequestPermissionResponse {
    use agent_client_protocol::schema::v1::{
        PermissionOptionKind, RequestPermissionOutcome, RequestPermissionResponse,
        SelectedPermissionOutcome,
    };
    if let Some(option) = request
        .options
        .iter()
        .find(|option| {
            matches!(
                option.kind,
                PermissionOptionKind::AllowOnce | PermissionOptionKind::AllowAlways
            )
        })
        .or_else(|| request.options.first())
    {
        return RequestPermissionResponse::new(RequestPermissionOutcome::Selected(
            SelectedPermissionOutcome::new(option.option_id.clone()),
        ));
    }
    RequestPermissionResponse::new(RequestPermissionOutcome::Cancelled)
}

fn sleep_timeout(
    duration: Duration,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
    let (sender, receiver) = futures::channel::oneshot::channel();
    thread::spawn(move || {
        thread::sleep(duration);
        let _ = sender.send(());
    });
    Box::pin(async move {
        let _ = receiver.await;
    })
}

pub fn parse_http_input(text: &str) -> Option<HttpInput> {
    let bytes = text.as_bytes();
    let mut found = None;
    for (index, _) in bytes.iter().enumerate().filter(|(_, byte)| **byte == b'{') {
        let slice = &text[index..];
        let mut deserializer = serde_json::Deserializer::from_str(slice);
        if let Ok(input) = HttpInput::deserialize(&mut deserializer) {
            found = Some(input);
        }
    }
    found
}

#[derive(Deserialize)]
struct HttpInputBatch {
    requests: Vec<HttpInput>,
}

/// Parse a JSON array, a `{"requests": [...]}` envelope, or one legacy input.
pub fn parse_http_inputs(text: &str) -> Option<Vec<HttpInput>> {
    let bytes = text.as_bytes();
    for (index, _) in bytes.iter().enumerate().filter(|(_, byte)| **byte == b'[') {
        let slice = &text[index..];
        let mut deserializer = serde_json::Deserializer::from_str(slice);
        if let Ok(inputs) = Vec::<HttpInput>::deserialize(&mut deserializer) {
            if !inputs.is_empty() {
                return Some(inputs);
            }
        }
    }

    for (index, _) in bytes.iter().enumerate().filter(|(_, byte)| **byte == b'{') {
        let slice = &text[index..];
        let mut deserializer = serde_json::Deserializer::from_str(slice);
        if let Ok(batch) = HttpInputBatch::deserialize(&mut deserializer) {
            if !batch.requests.is_empty() {
                return Some(batch.requests);
            }
        }
    }

    parse_http_input(text).map(|input| vec![input])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore]
    fn acp_ping() {
        let config = LlmConfig::from_env();
        let agent = LlmAgent::new(
            config,
            PathBuf::from("/tmp"),
            PathBuf::from("/tmp"),
            Vec::new(),
        );
        match agent.run_acp("Reply with exactly the single word pong. Do not use tools.") {
            Ok(text) => println!("ACP_PING {text}"),
            Err(err) => println!("ACP_PING_ERR {err}"),
        }
    }

    #[test]
    fn parses_json_inside_prose() {
        let text = "说明\n```json\n{\"method\":\"POST\",\"path\":\"/wp-json/batch/v1\",\"query\":[],\"headers\":[],\"cookies\":[],\"body\":{\"Object\":[[\"title\",{\"String\":[115,101,101,100]}]]},\"body_override\":null,\"operation_key\":null,\"pin_route\":true,\"exec_limit\":0,\"redqueen\":false,\"coverage_dump\":false}\n```";
        let input = parse_http_input(text).expect("json");
        assert_eq!(input.method, "POST");
        assert!(!input.coverage_dump);
    }
}
