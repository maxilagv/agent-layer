mod auth;
use anyhow::{Context, Result, bail, ensure};
use async_trait::async_trait;
pub use auth::{forget_api_key, login, logout, store_api_key};
use futures_util::StreamExt;
use layeragent_config::{Config, Model, Provider, Wire};
use layeragent_protocol::{Completion, Message, ToolCall, ToolDefinition, Usage, new_id};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::Mutex,
    time::{Duration, Instant},
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug)]
pub struct Delta {
    pub attempt: String,
    pub model: String,
    pub kind: String,
    pub text: String,
}
#[async_trait]
pub trait LanguageModel: Send + Sync {
    async fn infer(
        &self,
        config: &Config,
        selected: &str,
        messages: &[Message],
        tools: &[ToolDefinition],
        cancel: CancellationToken,
        stream: mpsc::Sender<Delta>,
    ) -> Result<Completion>;
}
pub struct Router {
    client: reqwest::Client,
    health: Mutex<BTreeMap<String, Instant>>,
}
impl Router {
    pub fn new() -> Result<Self> {
        Ok(Self {
            client: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(15))
                .timeout(Duration::from_secs(300))
                .pool_idle_timeout(Duration::from_secs(90))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            health: Mutex::new(BTreeMap::new()),
        })
    }
}
#[async_trait]
impl LanguageModel for Router {
    async fn infer(
        &self,
        config: &Config,
        selected: &str,
        messages: &[Message],
        tools: &[ToolDefinition],
        cancel: CancellationToken,
        stream: mpsc::Sender<Delta>,
    ) -> Result<Completion> {
        let mut errors = vec![];
        let estimated = estimate_tokens(messages, tools);
        for alias in std::iter::once(selected).chain(config.fallbacks.iter().map(String::as_str)) {
            ensure!(!cancel.is_cancelled(), "inference cancelled");
            if let Err(error) = config.require_model_egress(alias) {
                errors.push(format!("{alias}: {error}"));
                continue;
            }
            let m = config
                .models
                .get(alias)
                .context("selected model is not configured")?;
            if estimated > m.context_window * 4 / 5 {
                errors.push(format!("{alias}: context does not fit"));
                continue;
            }
            if let Some(cap) = config.max_cost_usd {
                let Some(input_price) = m.input_per_million else {
                    errors.push(format!("{alias}: input price required for monetary budget"));
                    continue;
                };
                let Some(output_price) = m.output_per_million else {
                    errors.push(format!(
                        "{alias}: output price required for monetary budget"
                    ));
                    continue;
                };
                let reserve = (estimated as f64 * input_price
                    + config.max_output_tokens.min(m.context_window / 5) as f64 * output_price)
                    / 1_000_000.0;
                if reserve > cap {
                    errors.push(format!(
                        "{alias}: monetary budget cannot reserve this request"
                    ));
                    continue;
                }
            }
            let p = config
                .providers
                .get(&m.provider)
                .context("provider is missing")?;
            if self
                .health
                .lock()
                .map_err(|_| anyhow::anyhow!("health lock poisoned"))?
                .get(&m.provider)
                .is_some_and(|t| t.elapsed() < Duration::from_secs(60))
            {
                errors.push(format!("{}: circuit open", m.provider));
                continue;
            }
            let attempt = new_id();
            let _ = stream
                .send(Delta {
                    attempt: attempt.clone(),
                    model: alias.into(),
                    kind: "attempt_started".into(),
                    text: String::new(),
                })
                .await;
            let result = tokio::select! {_=cancel.cancelled()=>bail!("inference cancelled"),r=self.request(p,m,messages,tools,config.max_output_tokens,(&attempt,alias),&stream)=>r};
            match result {
                Ok(mut completion) => {
                    if !m.tools && m.emulate_tools && !tools.is_empty() {
                        let mut value = serde_json::from_str::<Value>(&completion.text);
                        if value.is_err() {
                            let mut repair = messages.to_vec();
                            repair.push(Message::text("assistant", &completion.text));
                            repair.push(Message::text("user","Return only the valid JSON action envelope specified in the system instructions. Do not execute anything or repeat earlier actions."));
                            let repaired = self
                                .request(
                                    p,
                                    m,
                                    &repair,
                                    tools,
                                    config.max_output_tokens,
                                    (&attempt, alias),
                                    &stream,
                                )
                                .await?;
                            completion.usage.input_tokens += repaired.usage.input_tokens;
                            completion.usage.output_tokens += repaired.usage.output_tokens;
                            completion.text = repaired.text;
                            value = serde_json::from_str(&completion.text);
                        }
                        let v = value.context("model returned an invalid action envelope")?;
                        ensure!(v.is_object(), "action envelope must be an object");
                        for k in v.as_object().into_iter().flat_map(|o| o.keys()) {
                            ensure!(
                                ["text", "calls"].contains(&k.as_str()),
                                "unknown action envelope field"
                            );
                        }
                        completion.text = v
                            .get("text")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .into();
                        if let Some(calls) = v.get("calls") {
                            let calls = calls.as_array().context("calls must be an array")?;
                            ensure!(calls.len() <= 16, "too many tool calls");
                            for c in calls {
                                let name = c["name"].as_str().context("tool name required")?;
                                ensure!(
                                    tools.iter().any(|t| t.name == name),
                                    "unknown requested tool"
                                );
                                completion.calls.push(ToolCall {
                                    provider_data: serde_json::Value::Null,
                                    id: new_id(),
                                    name: name.into(),
                                    arguments: c
                                        .get("arguments")
                                        .cloned()
                                        .context("tool arguments required")?,
                                });
                            }
                        }
                    }
                    if !completion.provider_data.is_null() {
                        completion.provider_data["provider"] = json!(m.provider);
                    }
                    for call in &mut completion.calls {
                        if !call.provider_data.is_null() {
                            call.provider_data["provider"] = json!(m.provider);
                        }
                    }
                    if completion.usage.input_tokens == 0 {
                        completion.usage.input_tokens = estimated;
                    }
                    if completion.usage.output_tokens == 0 {
                        completion.usage.output_tokens = (completion.text.len() as u64 / 3).max(1);
                    }
                    if !m.tools && m.emulate_tools && !completion.text.is_empty() {
                        let _ = stream
                            .send(Delta {
                                attempt: attempt.clone(),
                                model: alias.into(),
                                kind: "text_delta".into(),
                                text: completion.text.clone(),
                            })
                            .await;
                    }
                    completion.model = alias.into();
                    return Ok(completion);
                }
                Err(e) => {
                    let transient = e
                        .downcast_ref::<ProviderFailure>()
                        .is_some_and(|e| e.transient);
                    if !transient {
                        return Err(e);
                    }
                    self.health
                        .lock()
                        .map_err(|_| anyhow::anyhow!("health lock poisoned"))?
                        .insert(m.provider.clone(), Instant::now());
                    let _ = stream
                        .send(Delta {
                            attempt,
                            model: alias.into(),
                            kind: "attempt_discarded".into(),
                            text: "Transient provider failure; trying an authorized fallback"
                                .into(),
                        })
                        .await;
                    errors.push(format!("{alias}: {e}"));
                }
            }
        }
        bail!("no authorized provider available: {}", errors.join("; "))
    }
}
#[derive(Debug)]
struct ProviderFailure {
    message: String,
    transient: bool,
}
impl std::fmt::Display for ProviderFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for ProviderFailure {}
impl Router {
    #[allow(clippy::too_many_arguments)]
    async fn request(
        &self,
        p: &Provider,
        m: &Model,
        messages: &[Message],
        tools: &[ToolDefinition],
        max: u64,
        attempt: (&str, &str),
        stream: &mpsc::Sender<Delta>,
    ) -> Result<Completion> {
        let credential = auth::credential(&self.client, &m.provider, p).await?;
        let messages = messages
            .iter()
            .map(layeragent_protocol::redact::sanitize_message)
            .collect::<Result<Vec<_>>>()?;
        let tools = layeragent_protocol::redact::sanitize_tools(tools)?;
        let (url, body) = format_request(p, m, &messages, &tools, max)?;
        let mut request = self.client.post(url).json(&body);
        if let Some(key) = credential {
            request = match p.wire {
                Wire::Anthropic => request.header("x-api-key", key),
                Wire::Gemini if p.oauth.is_none() || p.key_env.is_some() => {
                    request.header("x-goog-api-key", key)
                }
                _ => request.bearer_auth(key),
            };
        }
        if p.wire == Wire::Anthropic {
            request = request.header("anthropic-version", "2023-06-01");
        }
        for (key, value) in &p.headers {
            request = request.header(key, value);
        }
        let network_started = std::time::Instant::now();
        let response = request.send().await.map_err(|_| ProviderFailure {
            message: "provider connection failed".into(),
            transient: true,
        })?;
        let _ = stream
            .send(Delta {
                attempt: attempt.0.into(),
                model: attempt.1.into(),
                kind: "network_ms".into(),
                text: network_started.elapsed().as_millis().to_string(),
            })
            .await;
        let status = response.status();
        if !status.is_success() {
            return Err(ProviderFailure {
                message: format!("provider HTTP {}", status.as_u16()),
                transient: status.as_u16() == 429 || status.is_server_error(),
            }
            .into());
        }
        let is_json = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|s| s.starts_with("application/json"));
        let mut chunks = response.bytes_stream();
        let mut decoder = FrameDecoder::default();
        let mut accumulator = Accumulator::default();
        let mut total = 0;
        while let Some(chunk) = chunks.next().await {
            let chunk = chunk.map_err(|_| ProviderFailure {
                message: "provider stream interrupted".into(),
                transient: true,
            })?;
            total += chunk.len();
            ensure!(total <= 16 * 1024 * 1024, "provider response exceeds limit");
            if is_json {
                decoder.buffer.extend_from_slice(&chunk);
                continue;
            }
            let frames = decoder.push(&chunk, p.wire == Wire::Ollama)?;
            for frame in frames {
                if frame == "[DONE]" {
                    accumulator.done = true;
                    continue;
                }
                let event: Value =
                    serde_json::from_str(&frame).context("invalid provider stream event")?;
                let text = accumulator.push(&p.wire, &event)?;
                if !text.is_empty() && (m.tools || !m.emulate_tools || tools.is_empty()) {
                    let _ = stream
                        .send(Delta {
                            attempt: attempt.0.into(),
                            model: attempt.1.into(),
                            kind: "text_delta".into(),
                            text,
                        })
                        .await;
                }
            }
        }
        if !decoder.buffer.is_empty() {
            let tail = String::from_utf8(std::mem::take(&mut decoder.buffer))?;
            let tail = tail.trim();
            if !tail.is_empty() {
                let event: Value =
                    serde_json::from_str(tail.strip_prefix("data:").unwrap_or(tail).trim())
                        .context("truncated provider response")?;
                let text = if is_json {
                    accumulator.document(&p.wire, &event)?
                } else {
                    accumulator.push(&p.wire, &event)?
                };
                if !text.is_empty() && (m.tools || !m.emulate_tools || tools.is_empty()) {
                    let _ = stream
                        .send(Delta {
                            attempt: attempt.0.into(),
                            model: attempt.1.into(),
                            kind: "text_delta".into(),
                            text,
                        })
                        .await;
                }
            }
        }
        if !accumulator.done {
            return Err(ProviderFailure {
                message: "provider stream ended without a completion marker".into(),
                transient: true,
            }
            .into());
        }
        accumulator.finish()
    }
}
pub fn estimate_tokens(messages: &[Message], tools: &[ToolDefinition]) -> u64 {
    ((serde_json::to_vec(messages).map_or(0, |s| s.len())
        + serde_json::to_vec(tools).map_or(0, |s| s.len())) as u64)
        .div_ceil(3)
        + 32
}

pub fn format_request(
    p: &Provider,
    m: &Model,
    messages: &[Message],
    tools: &[ToolDefinition],
    max: u64,
) -> Result<(String, Value)> {
    let mut messages = messages.to_vec();
    if !m.tools && m.emulate_tools && !tools.is_empty() {
        messages.insert(0,Message::text("system",format!("Return exactly one JSON object with fields text (string) and calls (array of objects with name and arguments). To answer, use calls: []. Use only these tools: {}. Never emit shell commands outside this envelope.",serde_json::to_string(tools)?)));
    }
    if !m.tools {
        for message in &mut messages {
            if message.role == "tool" {
                message.role = "user".into();
                message.content = format!(
                    "Result of tool call {}: {}",
                    message.tool_call_id.as_deref().unwrap_or("unknown"),
                    message.content
                );
                message.tool_call_id = None;
            }
            if !message.tool_calls.is_empty() {
                message.content.push_str(&format!(
                    "\nPrior tool calls: {}",
                    serde_json::to_string(&message.tool_calls)?
                ));
                message.tool_calls.clear();
            }
            message.provider_data = Value::Null;
        }
    }
    let tools = if m.tools { tools } else { &[] };
    let base = p.base_url.trim_end_matches('/');
    let max = max.min(m.context_window / 5).max(1);
    let funcs=tools.iter().map(|t|json!({"type":"function","function":{"name":t.name,"description":t.description,"parameters":t.parameters}})).collect::<Vec<_>>();
    let result: Result<(String, Value)> = match p.wire {
        Wire::OpenaiChat => {
            let ms = messages.iter().map(|message| {
                let mut value = json!({"role":message.role,"content":message.content});
                if let Some(id) = &message.tool_call_id { value["tool_call_id"] = json!(id); }
                if !message.tool_calls.is_empty() {
                    value["tool_calls"] = json!(message.tool_calls.iter().map(|call| json!({"id":call.id,"type":"function","function":{"name":call.name,"arguments":call.arguments.to_string()}})).collect::<Vec<_>>());
                }
                if message.role == "assistant" && message.provider_data["wire"] == "openai_chat" && message.provider_data["provider"] == m.provider {
                    value["reasoning_content"] = message.provider_data["reasoning_content"].clone();
                }
                value
            }).collect::<Vec<_>>();
            let mut body =
                json!({"model":m.id,"messages":ms,"stream":true,"max_completion_tokens":max});
            if !funcs.is_empty() {
                body["tools"] = json!(funcs);
            }
            Ok((format!("{base}/chat/completions"), body))
        }
        Wire::OpenaiResponses => {
            let mut input = vec![];
            for msg in &messages {
                if msg.provider_data["wire"] == "openai_responses"
                    && msg.provider_data["provider"] == m.provider
                    && let Some(items) = msg.provider_data["items"].as_array()
                {
                    input.extend(items.clone());
                    continue;
                }
                if msg.role == "tool" {
                    input.push(json!({"type":"function_call_output","call_id":msg.tool_call_id,"output":msg.content}));
                } else {
                    if !msg.content.is_empty() {
                        input.push(json!({"role":msg.role,"content":msg.content}));
                    }
                    for c in &msg.tool_calls {
                        input.push(json!({"type":"function_call","call_id":c.id,"name":c.name,"arguments":c.arguments.to_string()}));
                    }
                }
            }
            Ok((
                format!("{base}/responses"),
                json!({"model":m.id,"input":input,"stream":true,"store":false,"include":["reasoning.encrypted_content"],"max_output_tokens":max,"tools":tools.iter().map(|t|json!({"type":"function","name":t.name,"description":t.description,"parameters":t.parameters})).collect::<Vec<_>>()}),
            ))
        }
        Wire::Anthropic => {
            let system = messages
                .iter()
                .filter(|m| m.role == "system")
                .map(|m| m.content.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            let mut ms: Vec<Value> = vec![];
            for msg in messages.iter().filter(|m| m.role != "system") {
                let (role, mut content) = if msg.role == "tool" {
                    (
                        "user",
                        vec![
                            json!({"type":"tool_result","tool_use_id":msg.tool_call_id,"content":msg.content}),
                        ],
                    )
                } else {
                    let native = msg.role == "assistant"
                        && msg.provider_data["wire"] == "anthropic"
                        && msg.provider_data["provider"] == m.provider;
                    let mut blocks = if native {
                        msg.provider_data["items"]
                            .as_array()
                            .cloned()
                            .context("invalid native Anthropic blocks")?
                    } else {
                        vec![]
                    };
                    if !native && !msg.content.is_empty() {
                        blocks.push(json!({"type":"text","text":msg.content}));
                    }
                    for c in msg.tool_calls.iter().filter(|_| !native) {
                        blocks.push(
                            json!({"type":"tool_use","id":c.id,"name":c.name,"input":c.arguments}),
                        );
                    }
                    (msg.role.as_str(), blocks)
                };
                if let Some(last) = ms.last_mut().filter(|v| v["role"] == role) {
                    last["content"]
                        .as_array_mut()
                        .context("invalid content")?
                        .append(&mut content);
                } else {
                    ms.push(json!({"role":role,"content":content}));
                }
            }
            Ok((
                format!("{base}/messages"),
                json!({"model":m.id,"system":system,"messages":ms,"max_tokens":max,"stream":true,"tools":tools.iter().map(|t|json!({"name":t.name,"description":t.description,"input_schema":t.parameters})).collect::<Vec<_>>()}),
            ))
        }
        Wire::Gemini => {
            let mut contents = vec![];
            let mut names = BTreeMap::new();
            for msg in &messages {
                if msg.role == "system" {
                    continue;
                }
                let mut parts = vec![];
                if msg.role == "tool" {
                    parts.push(json!({"functionResponse":{"name":names.get(msg.tool_call_id.as_deref().unwrap_or("")).cloned().unwrap_or_default(),"response":{"result":msg.content}}}));
                } else {
                    if !msg.content.is_empty() {
                        parts.push(json!({"text":msg.content}));
                    }
                    for c in &msg.tool_calls {
                        names.insert(c.id.clone(), c.name.clone());
                        parts.push(
                            if c.provider_data["wire"] == "gemini"
                                && c.provider_data["provider"] == m.provider
                            {
                                c.provider_data["part"].clone()
                            } else {
                                json!({"functionCall":{"name":c.name,"args":c.arguments}})
                            },
                        );
                    }
                }
                contents.push(
                    json!({"role":if msg.role=="assistant"{"model"}else{"user"},"parts":parts}),
                );
            }
            let mut body = json!({"contents":contents,"systemInstruction":{"parts":[{"text":messages.iter().filter(|m|m.role=="system").map(|m|m.content.as_str()).collect::<Vec<_>>().join("\n")}]},"generationConfig":{"maxOutputTokens":max}});
            if !tools.is_empty() {
                body["tools"] = json!([{"functionDeclarations":tools.iter().map(|t|json!({"name":t.name,"description":t.description,"parameters":t.parameters})).collect::<Vec<_>>()}]);
            }
            ensure!(
                m.id.bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"-._".contains(&c)),
                "invalid Gemini model id"
            );
            Ok((
                format!("{base}/models/{}:streamGenerateContent?alt=sse", m.id),
                body,
            ))
        }
        Wire::Ollama => {
            let ms = messages
                .iter()
                .map(|msg| {
                    let mut v = json!({"role":msg.role,"content":msg.content});
                    if !msg.tool_calls.is_empty() {
                        v["tool_calls"] = json!(
                            msg.tool_calls
                                .iter()
                                .map(
                                    |c| json!({"function":{"name":c.name,"arguments":c.arguments}})
                                )
                                .collect::<Vec<_>>()
                        );
                    }
                    v
                })
                .collect::<Vec<_>>();
            Ok((
                format!("{base}/api/chat"),
                json!({"model":m.id,"messages":ms,"stream":true,"tools":funcs,"options":{"num_predict":max}}),
            ))
        }
    };
    let (url, mut body) = result?;
    for (key, value) in &m.parameters {
        body[key] = value.clone();
    }
    match p.wire {
        Wire::OpenaiChat => {
            let field = if m.parameters.contains_key("max_completion_tokens")
                || (!m.parameters.contains_key("max_tokens")
                    && p.base_url.starts_with("https://api.openai.com/"))
            {
                "max_completion_tokens"
            } else {
                "max_tokens"
            };
            body.as_object_mut()
                .context("invalid request object")?
                .remove(if field == "max_tokens" {
                    "max_completion_tokens"
                } else {
                    "max_tokens"
                });
            body[field] = json!(
                m.parameters
                    .get(field)
                    .and_then(Value::as_u64)
                    .unwrap_or(max)
                    .min(max)
            );
        }
        Wire::OpenaiResponses => {
            body["max_output_tokens"] =
                json!(body["max_output_tokens"].as_u64().unwrap_or(max).min(max))
        }
        Wire::Anthropic => {
            body["max_tokens"] = json!(body["max_tokens"].as_u64().unwrap_or(max).min(max))
        }
        Wire::Gemini => {
            if !body["generationConfig"].is_object() {
                body["generationConfig"] = json!({});
            }
            body["generationConfig"]["maxOutputTokens"] = json!(
                body["generationConfig"]["maxOutputTokens"]
                    .as_u64()
                    .unwrap_or(max)
                    .min(max)
            );
        }
        Wire::Ollama => {
            if !body["options"].is_object() {
                body["options"] = json!({});
            }
            body["options"]["num_predict"] = json!(
                body["options"]["num_predict"]
                    .as_u64()
                    .unwrap_or(max)
                    .min(max)
            );
        }
    }
    Ok((url, body))
}

#[derive(Default)]
pub struct FrameDecoder {
    buffer: Vec<u8>,
}
impl FrameDecoder {
    pub fn push(&mut self, bytes: &[u8], ndjson: bool) -> Result<Vec<String>> {
        self.buffer.extend_from_slice(bytes);
        ensure!(
            self.buffer.len() <= 2 * 1024 * 1024,
            "provider frame too large"
        );
        let mut frames = vec![];
        loop {
            let end = if ndjson {
                self.buffer.iter().position(|b| *b == b'\n').map(|i| (i, 1))
            } else {
                let lf = self
                    .buffer
                    .windows(2)
                    .position(|w| w == b"\n\n")
                    .map(|i| (i, 2));
                let crlf = self
                    .buffer
                    .windows(4)
                    .position(|w| w == b"\r\n\r\n")
                    .map(|i| (i, 4));
                // TCP may coalesce events using different legal line endings.
                // Honor stream order instead of preferring one delimiter style.
                lf.into_iter().chain(crlf).min_by_key(|(offset, _)| *offset)
            };
            let Some((at, len)) = end else { break };
            let frame = String::from_utf8(self.buffer.drain(..at + len).collect())?;
            let data = if ndjson {
                frame.trim().to_string()
            } else {
                frame
                    .lines()
                    .filter_map(|l| {
                        l.strip_prefix("data:")
                            .map(|s| s.strip_prefix(' ').unwrap_or(s))
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            if !data.is_empty() {
                frames.push(data);
            }
        }
        Ok(frames)
    }
}
#[derive(Default)]
struct PartialCall {
    id: String,
    name: String,
    args: String,
}
#[derive(Default)]
struct Accumulator {
    text: String,
    calls: BTreeMap<usize, PartialCall>,
    ready: Vec<ToolCall>,
    usage: Usage,
    done: bool,
    native: Vec<Value>,
    blocks: BTreeMap<usize, Value>,
    reasoning: String,
}
impl Accumulator {
    fn document(&mut self, wire: &Wire, v: &Value) -> Result<String> {
        ensure!(v.get("error").is_none(), "provider returned an error");
        match wire {
            Wire::OpenaiResponses => {
                ensure!(
                    v["status"] == "completed",
                    "provider did not complete the response"
                );
                let mut text = String::new();
                for item in v["output"].as_array().into_iter().flatten() {
                    self.push(
                        wire,
                        &json!({"type":"response.output_item.done","item":item}),
                    )?;
                    for block in item["content"].as_array().into_iter().flatten() {
                        if block["type"] == "output_text" {
                            text.push_str(block["text"].as_str().unwrap_or_default());
                        }
                    }
                }
                self.text.push_str(&text);
                self.push(
                    wire,
                    &json!({"type":"response.completed","response":{"usage":v["usage"]}}),
                )?;
                Ok(text)
            }
            Wire::Anthropic => {
                self.push(
                    wire,
                    &json!({"type":"message_start","message":{"usage":v["usage"]}}),
                )?;
                let mut text = String::new();
                for (index, block) in v["content"].as_array().into_iter().flatten().enumerate() {
                    self.push(
                        wire,
                        &json!({"type":"content_block_start","index":index,"content_block":block}),
                    )?;
                    if block["type"] == "text" {
                        text.push_str(block["text"].as_str().unwrap_or_default());
                    }
                    if block["type"] == "tool_use" {
                        self.calls
                            .get_mut(&index)
                            .context("tool start missing")?
                            .args = block["input"].to_string();
                    }
                }
                self.text.push_str(&text);
                self.push(wire,&json!({"type":"message_delta","delta":{"stop_reason":v["stop_reason"]},"usage":v["usage"]}))?;
                self.done = true;
                Ok(text)
            }
            _ => self.push(wire, v),
        }
    }
    fn push(&mut self, wire: &Wire, v: &Value) -> Result<String> {
        if v.get("error").is_some() || v["type"] == "error" {
            bail!("provider returned a stream error");
        }
        let mut text = String::new();
        match wire {
            Wire::OpenaiChat => {
                for choice in v["choices"].as_array().into_iter().flatten() {
                    let delta = choice
                        .get("delta")
                        .or_else(|| choice.get("message"))
                        .unwrap_or(&Value::Null);
                    if let Some(t) = delta["content"].as_str() {
                        text.push_str(t);
                    }
                    if let Some(t) = delta["reasoning_content"].as_str() {
                        self.reasoning.push_str(t);
                    }
                    for (position, call) in delta["tool_calls"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .enumerate()
                    {
                        let index = call["index"].as_u64().unwrap_or(position as u64) as usize;
                        let c = self.calls.entry(index).or_default();
                        if let Some(id) = call["id"].as_str() {
                            c.id = id.into();
                        }
                        if let Some(name) = call["function"]["name"].as_str() {
                            c.name.push_str(name);
                        }
                        if let Some(args) = call["function"]["arguments"].as_str() {
                            c.args.push_str(args);
                        }
                    }
                    if !choice["finish_reason"].is_null() {
                        ensure!(
                            !["length", "content_filter"]
                                .contains(&choice["finish_reason"].as_str().unwrap_or("")),
                            "provider did not complete this generation"
                        );
                        self.done = true;
                    }
                }
                self.usage.input_tokens = v["usage"]["prompt_tokens"]
                    .as_u64()
                    .unwrap_or(self.usage.input_tokens);
                self.usage.output_tokens = v["usage"]["completion_tokens"]
                    .as_u64()
                    .unwrap_or(self.usage.output_tokens);
            }
            Wire::OpenaiResponses => match v["type"].as_str().unwrap_or("") {
                "response.output_text.delta" => text.push_str(v["delta"].as_str().unwrap_or("")),
                "response.output_item.done" => {
                    let item = &v["item"];
                    self.native.push(item.clone());
                    if item["type"] == "function_call" {
                        self.ready.push(ToolCall {
                            provider_data: serde_json::Value::Null,
                            id: item["call_id"].as_str().context("missing call id")?.into(),
                            name: item["name"].as_str().context("missing name")?.into(),
                            arguments: serde_json::from_str(
                                item["arguments"].as_str().context("missing arguments")?,
                            )?,
                        });
                    }
                }
                "response.completed" => {
                    self.done = true;
                    self.usage.input_tokens =
                        v["response"]["usage"]["input_tokens"].as_u64().unwrap_or(0);
                    self.usage.output_tokens = v["response"]["usage"]["output_tokens"]
                        .as_u64()
                        .unwrap_or(0);
                }
                "response.failed" | "response.incomplete" => {
                    bail!("provider did not complete the response")
                }
                _ => {}
            },
            Wire::Anthropic => {
                let index = v["index"].as_u64().unwrap_or(0) as usize;
                match v["type"].as_str().unwrap_or("") {
                    "message_start" => {
                        self.usage.input_tokens =
                            v["message"]["usage"]["input_tokens"].as_u64().unwrap_or(0)
                    }
                    "content_block_start" => {
                        let block = &v["content_block"];
                        self.blocks.insert(index, block.clone());
                        if block["type"] == "tool_use" {
                            self.calls.insert(
                                index,
                                PartialCall {
                                    id: block["id"].as_str().context("missing id")?.into(),
                                    name: block["name"].as_str().context("missing name")?.into(),
                                    args: String::new(),
                                },
                            );
                        }
                    }
                    "content_block_delta" => {
                        if let Some(block) = self.blocks.get_mut(&index) {
                            for field in ["text", "thinking", "signature"] {
                                if let Some(delta) = v["delta"][field].as_str() {
                                    let prior = block[field].as_str().unwrap_or_default();
                                    block[field] = json!(format!("{prior}{delta}"));
                                }
                            }
                        }
                        if let Some(t) = v["delta"]["text"].as_str() {
                            text.push_str(t);
                        }
                        if let Some(t) = v["delta"]["partial_json"].as_str() {
                            self.calls
                                .get_mut(&index)
                                .context("tool delta without start")?
                                .args
                                .push_str(t);
                        }
                    }
                    "message_delta" => {
                        ensure!(
                            v["delta"]["stop_reason"] != "max_tokens",
                            "provider output limit reached"
                        );
                        self.usage.output_tokens = v["usage"]["output_tokens"].as_u64().unwrap_or(0)
                    }
                    "message_stop" => self.done = true,
                    _ => {}
                }
            }
            Wire::Gemini => {
                ensure!(
                    v["promptFeedback"].get("blockReason").is_none(),
                    "provider declined this request"
                );
                for candidate in v["candidates"].as_array().into_iter().flatten() {
                    for part in candidate["content"]["parts"]
                        .as_array()
                        .into_iter()
                        .flatten()
                    {
                        if part["thought"] != true
                            && let Some(t) = part["text"].as_str()
                        {
                            text.push_str(t);
                        }
                        if let Some(call) = part.get("functionCall") {
                            self.ready.push(ToolCall {
                                provider_data: json!({"wire":"gemini","part":part}),
                                id: new_id(),
                                name: call["name"].as_str().context("missing tool name")?.into(),
                                arguments: call["args"].clone(),
                            });
                        }
                    }
                    if candidate.get("finishReason").is_some() {
                        ensure!(
                            candidate["finishReason"] == "STOP",
                            "provider stopped without completing the generation"
                        );
                        self.done = true;
                    }
                }
                self.usage.input_tokens = v["usageMetadata"]["promptTokenCount"]
                    .as_u64()
                    .unwrap_or(self.usage.input_tokens);
                self.usage.output_tokens = v["usageMetadata"]["candidatesTokenCount"]
                    .as_u64()
                    .unwrap_or(self.usage.output_tokens);
            }
            Wire::Ollama => {
                if let Some(t) = v["message"]["content"].as_str() {
                    text.push_str(t);
                }
                for call in v["message"]["tool_calls"].as_array().into_iter().flatten() {
                    self.ready.push(ToolCall {
                        provider_data: serde_json::Value::Null,
                        id: new_id(),
                        name: call["function"]["name"]
                            .as_str()
                            .context("missing tool name")?
                            .into(),
                        arguments: call["function"]["arguments"].clone(),
                    });
                }
                if v["done"] == true {
                    ensure!(
                        v["done_reason"] != "length",
                        "provider output limit reached"
                    );
                    self.done = true;
                    self.usage.input_tokens = v["prompt_eval_count"].as_u64().unwrap_or(0);
                    self.usage.output_tokens = v["eval_count"].as_u64().unwrap_or(0);
                }
            }
        }
        self.text.push_str(&text);
        Ok(text)
    }
    fn finish(mut self) -> Result<Completion> {
        for (index, call) in self.calls {
            let arguments: Value = serde_json::from_str(if call.args.is_empty() {
                "{}"
            } else {
                &call.args
            })
            .context("invalid tool arguments")?;
            if let Some(block) = self.blocks.get_mut(&index) {
                block["input"] = arguments.clone();
            }
            self.ready.push(ToolCall {
                provider_data: serde_json::Value::Null,
                id: if call.id.is_empty() {
                    new_id()
                } else {
                    call.id
                },
                name: call.name,
                arguments,
            });
        }
        ensure!(self.ready.len() <= 16, "too many tool calls");
        Ok(Completion {
            text: self.text,
            calls: self.ready,
            usage: self.usage,
            model: String::new(),
            provider_data: if !self.native.is_empty() {
                json!({"wire":"openai_responses","items":self.native})
            } else if !self.blocks.is_empty() {
                json!({"wire":"anthropic","items":self.blocks.into_values().collect::<Vec<_>>()})
            } else if !self.reasoning.is_empty() {
                json!({"wire":"openai_chat","reasoning_content":self.reasoning})
            } else {
                Value::Null
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fragmented_unicode_and_crlf() -> Result<()> {
        let frame = "data: {\"text\":\"acción 🦀\"}\r\n\r\n".as_bytes();
        for n in 1..frame.len() {
            let mut d = FrameDecoder::default();
            let mut frames = d.push(&frame[..n], false)?;
            frames.extend(d.push(&frame[n..], false)?);
            assert_eq!(frames, vec!["{\"text\":\"acción 🦀\"}"]);
        }
        Ok(())
    }
    #[test]
    fn fragmented_tool_arguments() -> Result<()> {
        let mut a = Accumulator::default();
        a.push(&Wire::OpenaiChat,&json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c","function":{"name":"read_file","arguments":"{\"path\":"}}]}}]}))?;
        a.push(&Wire::OpenaiChat,&json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"a\"}"}}]},"finish_reason":"tool_calls"}]}))?;
        assert_eq!(a.finish()?.calls[0].arguments, json!({"path":"a"}));
        Ok(())
    }
    #[test]
    fn incomplete_and_declined_generations_are_not_successful() {
        for (wire, value) in [
            (
                Wire::OpenaiChat,
                json!({"choices":[{"finish_reason":"length"}]}),
            ),
            (
                Wire::OpenaiChat,
                json!({"choices":[{"finish_reason":"content_filter"}]}),
            ),
            (Wire::OpenaiResponses, json!({"type":"response.incomplete"})),
            (
                Wire::Anthropic,
                json!({"type":"message_delta","delta":{"stop_reason":"max_tokens"}}),
            ),
            (
                Wire::Gemini,
                json!({"promptFeedback":{"blockReason":"SAFETY"}}),
            ),
            (
                Wire::Gemini,
                json!({"candidates":[{"finishReason":"MAX_TOKENS"}]}),
            ),
            (Wire::Ollama, json!({"done":true,"done_reason":"length"})),
        ] {
            assert!(Accumulator::default().push(&wire, &value).is_err());
        }
    }
    #[test]
    fn non_streaming_documents_support_multiple_calls_and_native_reasoning() -> Result<()> {
        let mut chat = Accumulator::default();
        chat.document(&Wire::OpenaiChat,&json!({"choices":[{"message":{"content":"","reasoning_content":"private reasoning","tool_calls":[{"id":"a","function":{"name":"first","arguments":"{}"}},{"id":"b","function":{"name":"second","arguments":"{\"x\":1}"}}]},"finish_reason":"tool_calls"}]}))?;
        let completion = chat.finish()?;
        assert_eq!(completion.calls.len(), 2);
        assert_eq!(completion.calls[1].name, "second");
        assert_eq!(
            completion.provider_data["reasoning_content"],
            "private reasoning"
        );
        let mut anthropic = Accumulator::default();
        anthropic.document(&Wire::Anthropic,&json!({"type":"message","stop_reason":"tool_use","content":[{"type":"thinking","thinking":"analysis","signature":"signed"},{"type":"tool_use","id":"a","name":"first","input":{"x":1}}],"usage":{"input_tokens":10,"output_tokens":5}}))?;
        let completion = anthropic.finish()?;
        assert_eq!(completion.calls[0].arguments, json!({"x":1}));
        assert_eq!(completion.provider_data["items"][0]["signature"], "signed");
        assert_eq!(completion.usage.output_tokens, 5);
        let mut responses = Accumulator::default();
        responses.document(&Wire::OpenaiResponses,&json!({"status":"completed","output":[{"type":"reasoning","encrypted_content":"encrypted"},{"type":"message","role":"assistant","content":[{"type":"output_text","text":"finished"}]}],"usage":{"input_tokens":10,"output_tokens":3}}))?;
        assert_eq!(responses.finish()?.text, "finished");
        Ok(())
    }
    #[test]
    fn anthropic_stream_keeps_signed_thinking_with_the_matching_provider() -> Result<()> {
        let mut a = Accumulator::default();
        for event in [
            json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"analysis"}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"signed"}}),
            json!({"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"a","name":"first","input":{}}}),
            json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"x\":1}"}}),
            json!({"type":"message_stop"}),
        ] {
            a.push(&Wire::Anthropic, &event)?;
        }
        let mut completion = a.finish()?;
        completion.provider_data["provider"] = json!("fixture");
        let mut message = Message::text("assistant", "");
        message.tool_calls = completion.calls;
        message.provider_data = completion.provider_data;
        let provider = Provider {
            wire: Wire::Anthropic,
            base_url: "https://api.example.test".into(),
            key_env: None,
            oauth: None,
            headers: Default::default(),
        };
        let mut model = Model {
            provider: "fixture".into(),
            id: "model".into(),
            context_window: 8192,
            tools: true,
            emulate_tools: false,
            input_per_million: None,
            output_per_million: None,
            parameters: Default::default(),
        };
        let (_, body) =
            format_request(&provider, &model, std::slice::from_ref(&message), &[], 100)?;
        assert_eq!(body["messages"][0]["content"][0]["signature"], "signed");
        assert_eq!(body["messages"][0]["content"][1]["input"], json!({"x":1}));
        model.provider = "other".into();
        let (_, body) = format_request(&provider, &model, &[message], &[], 100)?;
        assert_eq!(body["messages"][0]["content"][0]["type"], "tool_use");
        Ok(())
    }
}
