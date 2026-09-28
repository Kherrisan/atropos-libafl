use std::{env, path::PathBuf, thread, time::Duration};

use libafl::{
    corpus::CorpusId,
    executors::ExitKind,
    fuzzer::{Evaluator, ExecutesInput},
    mutators::Mutator,
    state::HasRand,
    Error,
};
use libafl_bolts::rands::Rand;
use serde::Deserialize;

use crate::{
    coverage_report::{self, CoverageReport},
    input::HttpInput,
    mutate::AtroposMutator,
};

pub struct LlmConfig {
    pub provider: String,
    pub auth_file: Option<PathBuf>,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub model: Option<String>,
    pub stall: u64,
    pub probability: f64,
    pub timeout: Duration,
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
                .unwrap_or(0.8),
            timeout: Duration::from_secs(
                env::var("ATROPOS_LLM_TIMEOUT")
                    .ok()
                    .and_then(|text| text.parse().ok())
                    .unwrap_or(180),
            ),
        }
    }
}

pub struct LlmAgent {
    config: LlmConfig,
    execs_since_novel: u64,
    queue: Vec<String>,
}

impl LlmAgent {
    pub fn new(config: LlmConfig) -> Self {
        Self {
            config,
            execs_since_novel: 0,
            queue: Vec::new(),
        }
    }

    pub fn execs_since_novel(&self) -> u64 {
        self.execs_since_novel
    }

    pub fn note(&mut self, corpus_id: Option<CorpusId>, input: &HttpInput) {
        if corpus_id.is_some() {
            self.execs_since_novel = 0;
        } else {
            self.execs_since_novel = self.execs_since_novel.saturating_add(1);
        }
        self.queue.push(input.summary());
        if self.queue.len() > 12 {
            self.queue.remove(0);
        }
    }

    pub fn should_fire<S: HasRand>(&self, state: &mut S) -> bool {
        self.execs_since_novel >= self.config.stall
            && state.rand_mut().coinflip(self.config.probability)
    }

    /// Run the current input, dump source lines, ask the agent for a new input, then run that input.
    pub fn mutate_and_run<E, EM, S, Z>(
        &mut self,
        fuzzer: &mut Z,
        executor: &mut E,
        state: &mut S,
        manager: &mut EM,
        havoc: &mut AtroposMutator,
        input: &mut HttpInput,
    ) -> Result<(), Error>
    where
        S: HasRand,
        Z: ExecutesInput<E, EM, HttpInput, S> + Evaluator<E, EM, HttpInput, S>,
    {
        let mut traced = input.clone();
        traced.coverage_dump = true;
        traced.redqueen = false;
        let _exit: ExitKind = fuzzer.execute_input(state, executor, manager, &traced)?;
        let report = coverage_report::write_report(input, &self.queue)
            .map_err(|err| Error::unknown(format!("coverage report: {err}")))?;
        eprintln!("llm coverage hits written, new lines {}", report.new_lines);

        match self.ask(&report, input) {
            Ok(mut proposed) => {
                if input.pin_route {
                    proposed.method = input.method.clone();
                    proposed.path = input.path.clone();
                    proposed.pin_route = true;
                }
                proposed.coverage_dump = false;
                proposed.operation_key = input.operation_key.clone();
                *input = proposed;
                eprintln!("llm proposed {}", input.summary());
            }
            Err(err) => {
                eprintln!("llm fallback: {err}");
                havoc.mutate(state, input)?;
                input.coverage_dump = false;
            }
        }

        let (_, corpus_id) = fuzzer.evaluate_input(state, executor, manager, input)?;
        havoc.post_exec(state, corpus_id)?;
        self.note(corpus_id, input);
        Ok(())
    }

    fn ask(&self, report: &CoverageReport, current: &HttpInput) -> Result<HttpInput, String> {
        let mut prompt = render_prompt(report, current);
        if let Some(model) = &self.config.model {
            prompt = format!("使用模型 {model}。\n{prompt}");
        }
        let output = self.run_acp(&prompt)?;
        parse_http_input(&output).ok_or_else(|| "agent output had no HttpInput JSON".to_string())
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
}

fn render_prompt(report: &CoverageReport, current: &HttpInput) -> String {
    let current_json = serde_json::to_string_pretty(current).unwrap_or_else(|_| "{}".to_string());
    let wordpress_root = crate::paths::wordpress_root();
    format!(
        r#"你是覆盖导向的 HTTP 输入变异器。不要修改任何文件，不要执行写入。只在最终回复里打印一个 JSON 对象。

当前输入没有再走出新的语料。请阅读这次执行命中的 PHP 源码，把输入改成更可能进入尚未命中分支的请求。
源码根目录是 {wordpress_root}。入口是 index.php，REST 由 WordPress 的 wp() 分发。
若 pin_route 为 true，保持 method 和 path 不变，只改 query、headers、cookies 或 JSON body。

本次命中行：{hits}
campaign 已见行：{seen}
最近队列：{queue}
当前输入文件：{current_file}

当前输入：
{current_json}

最终回复只包含一个 JSON 对象，字段为 method, path, query, headers, cookies, body, body_override, operation_key, pin_route, exec_limit, redqueen, coverage_dump。
query、headers、cookies 是 [字符串, 字节数组] 的列表。body 是 JSON 值的树：Null、Bool、Number 字符串、String 字节数组、Array、Object 字段列表。coverage_dump 必须是 false。
"#,
        hits = report.hits_path.display(),
        seen = report.seen_path.display(),
        queue = report.queue_path.display(),
        current_file = report.current_path.display(),
        current_json = current_json,
        wordpress_root = wordpress_root.display(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore]
    fn acp_ping() {
        let config = LlmConfig::from_env();
        let agent = LlmAgent::new(config);
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
