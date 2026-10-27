use anyhow::{Context, Result, ensure};
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use tokio::sync::mpsc;

#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    base: String,
}

/// Where the daemon listens when UNIX sockets are unavailable: loopback TCP plus a
/// secret that only the current user's profile directory discloses.
#[cfg_attr(unix, allow(dead_code))]
#[derive(serde::Serialize, serde::Deserialize)]
pub struct Endpoint {
    pub port: u16,
    pub token: String,
}
#[cfg_attr(unix, allow(dead_code))]
pub fn endpoint_path(home: &Path, profile: &str) -> PathBuf {
    home.join("runtime").join(format!("{profile}.endpoint"))
}
impl Client {
    async fn http_for(home: &Path, profile: &str) -> Result<(reqwest::Client, String)> {
        layeragent_protocol::validate_name(profile)?;
        #[cfg(unix)]
        {
            let http = reqwest::Client::builder()
                .unix_socket(home.join("runtime").join(format!("{profile}.sock")))
                .timeout(std::time::Duration::from_secs(86400))
                .build()?;
            Ok((http, "http://localhost".into()))
        }
        #[cfg(not(unix))]
        {
            let builder = reqwest::Client::builder().timeout(std::time::Duration::from_secs(86400));
            match std::fs::read_to_string(endpoint_path(home, profile))
                .ok()
                .and_then(|text| serde_json::from_str::<Endpoint>(&text).ok())
            {
                Some(endpoint) => {
                    let mut headers = reqwest::header::HeaderMap::new();
                    headers.insert(
                        reqwest::header::AUTHORIZATION,
                        format!("Bearer {}", endpoint.token).parse()?,
                    );
                    Ok((
                        builder.default_headers(headers).build()?,
                        format!("http://127.0.0.1:{}", endpoint.port),
                    ))
                }
                // No daemon yet: this client only answers the readiness probe with an error.
                None => Ok((builder.build()?, "http://127.0.0.1:9".into())),
            }
        }
    }
    pub async fn connect(home: &Path, profile: &str) -> Result<Self> {
        let (http, base) = Self::http_for(home, profile).await?;
        #[cfg_attr(unix, allow(unused_mut))]
        let mut client = Self { http, base };
        if !tokio::time::timeout(std::time::Duration::from_secs(1), client.get("/health"))
            .await
            .is_ok_and(|r| r.is_ok())
        {
            let runtime = home.join("runtime");
            layeragent_config::private_dir(&runtime)?;
            let log = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(runtime.join(format!("{profile}.log")))?;
            let mut command = std::process::Command::new(std::env::current_exe()?);
            command
                .arg("--home")
                .arg(home)
                .arg("--profile")
                .arg(profile)
                .args(["serve", "--ipc-only"])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(log);
            #[cfg(unix)]
            {
                use std::os::unix::process::CommandExt;
                command.process_group(0);
            }
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                // DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW
                command.creation_flags(0x0000_0008 | 0x0000_0200 | 0x0800_0000);
            }
            let mut child = command.spawn()?;
            for _ in 0..200 {
                if tokio::time::timeout(
                    std::time::Duration::from_millis(500),
                    client.get("/health"),
                )
                .await
                .is_ok_and(|r| r.is_ok())
                {
                    return Ok(client);
                }
                #[cfg(not(unix))]
                if let Ok((http, base)) = Self::http_for(home, profile).await {
                    client = Self { http, base };
                }
                if let Some(status) = child.try_wait()? {
                    anyhow::bail!(
                        "daemon exited ({status}); inspect {}",
                        runtime.join(format!("{profile}.log")).display()
                    );
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
            anyhow::bail!("daemon did not become ready");
        }
        Ok(client)
    }
    pub async fn get(&self, path: &str) -> Result<Value> {
        let response = self.http.get(format!("{}{path}", self.base)).send().await?;
        ensure!(
            response.status().is_success(),
            "API request failed: {}",
            response.status()
        );
        Ok(response.json().await?)
    }
    pub async fn post(&self, path: &str, body: Value) -> Result<Value> {
        let response = self
            .http
            .post(format!("{}{path}", self.base))
            .json(&body)
            .send()
            .await?;
        let status = response.status();
        let value: Value = response.json().await?;
        ensure!(
            status.is_success(),
            "{}",
            value["error"].as_str().unwrap_or("API request failed")
        );
        Ok(value)
    }
    pub async fn session(&self, id: Option<String>, workspace: PathBuf) -> Result<String> {
        if let Some(id) = id {
            return Ok(id);
        }
        let value = self
            .post(
                "/v1/sessions",
                json!({"workspace":workspace.canonicalize()?.to_string_lossy()}),
            )
            .await?;
        Ok(value["id"].as_str().context("missing session id")?.into())
    }
    /// Submits the prompt as a durable task and follows its persisted event
    /// log. Reconnecting with the last sequence never repeats execution.
    pub async fn turn(
        &self,
        session: &str,
        prompt: &str,
        model: Option<String>,
        tx: mpsc::Sender<Value>,
    ) -> Result<()> {
        let key = layeragent_protocol::new_id();
        let created = self
            .http
            .post(format!("{}/v1/tasks", self.base))
            .header("idempotency-key", &key)
            .json(&json!({
                "session_id": session,
                "objective": prompt,
                "model": model,
            }))
            .send()
            .await?;
        let status = created.status();
        let body: Value = created.json().await?;
        ensure!(
            status.is_success(),
            "{}",
            body["error"].as_str().unwrap_or("task rejected")
        );
        let task = body["id"]
            .as_str()
            .context("task response has no id")?
            .to_string();
        if tx
            .send(json!({"job_id": task, "task_id": task, "session_id": session}))
            .await
            .is_err()
        {
            return Ok(());
        }
        let mut after = 0i64;
        let mut reconnects = 0;
        loop {
            let response = self
                .http
                .get(format!(
                    "{}/v1/tasks/{task}/events/stream?after={after}",
                    self.base
                ))
                .send()
                .await?;
            ensure!(
                response.status().is_success(),
                "task stream rejected: {}",
                response.status()
            );
            let mut bytes = response.bytes_stream();
            let mut decoder = layeragent_providers::FrameDecoder::default();
            while let Some(chunk) = bytes.next().await {
                for frame in decoder.push(&chunk?, false)? {
                    if frame.trim().is_empty() {
                        continue;
                    }
                    let event: Value = serde_json::from_str(&frame)?;
                    if let Some(seq) = event["seq"].as_i64() {
                        after = seq;
                    }
                    let kind = event["kind"].as_str().unwrap_or_default().to_string();
                    let terminal = matches!(
                        kind.as_str(),
                        "turn_completed" | "turn_failed" | "failed" | "interrupted"
                    );
                    // Notifications are best effort: a closed receiver means the
                    // caller stopped listening, not that the task is cancelled.
                    if tx.send(event.clone()).await.is_err() {
                        return Ok(());
                    }
                    if kind == "turn_completed" {
                        let _ = tx
                            .send(json!({
                                "text": event["data"]["text"].clone(),
                                "session_id": session,
                                "task_id": task,
                            }))
                            .await;
                        return Ok(());
                    }
                    if matches!(kind.as_str(), "failed" | "interrupted") {
                        let error = event["data"]["error"]
                            .as_str()
                            .unwrap_or("task failed")
                            .to_string();
                        let _ = tx.send(json!({"error": error})).await;
                        return Ok(());
                    }
                    if terminal {
                        return Ok(());
                    }
                }
            }
            // The stream ended before a terminal event (daemon restart or a
            // dropped connection). Only reattach when the task is still live.
            let task_state = self.get(&format!("/v1/tasks/{task}")).await?;
            match task_state["state"].as_str() {
                Some("completed") | Some("failed") | Some("cancelled") | Some("blocked") => {
                    return Ok(());
                }
                _ => {}
            }
            reconnects += 1;
            ensure!(reconnects <= 240, "task stream did not resume");
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
    }
    pub async fn print_turn(
        &self,
        session: &str,
        prompt: &str,
        model: Option<String>,
        json_output: bool,
    ) -> Result<()> {
        use std::io::{IsTerminal, Write};
        let (tx, mut rx) = mpsc::channel(64);
        let client = self.clone();
        let id = session.to_string();
        let prompt = prompt.to_string();
        let task = tokio::spawn(async move { client.turn(&id, &prompt, model, tx).await });
        let mut job: Option<String> = None;
        let mut failure = None;
        let mut completed = false;
        let mut rendered = false;
        let mut cancelled = false;
        eprintln!("Session: {session}");
        loop {
            tokio::select! {
                _=tokio::signal::ctrl_c()=>{
                    cancelled=true;
                    if let Some(id)=&job{let _=self.post(&format!("/v1/tasks/{id}/cancel"),json!({})).await;}
                },
                event=rx.recv()=>{
                    let Some(event)=event else{break};
                    if json_output{println!("{event}");}
                    if let Some(id)=event["job_id"].as_str(){job=Some(id.to_string());if cancelled{let _=self.post(&format!("/v1/tasks/{id}/cancel"),json!({})).await;}}
                    if event["kind"]=="approval_required"{
                        let id=event["data"]["approval_id"].as_str().unwrap_or("");
                        let allow=if !json_output && std::io::stdin().is_terminal() && std::io::stderr().is_terminal(){
                            match layeragent_ui::app::approval_review(&event["data"]) {
                                Ok(review) => {
                                    approval_key(&review).await?
                                }
                                Err(error) => { eprintln!("{error}"); false }
                            }
                        }else{false};
                        self.post(&format!("/v1/approvals/{id}"),json!({
                            "allow": allow,
                            "expected_revision": event["data"]["expected_revision"].as_i64().unwrap_or(0),
                            "arguments_hash": event["data"]["arguments_hash"].as_str().unwrap_or_default(),
                        })).await?;
                    }
                    if !json_output && event["kind"]=="text_delta"{
                        print!("{}",event["data"]["text"].as_str().unwrap_or(""));std::io::stdout().flush()?;rendered=true;
                    }
                    if !json_output && event["kind"]=="tool_output"{
                        eprint!("{}",event["data"]["text"].as_str().unwrap_or(""));
                    }
                    if !json_output && event["kind"]=="attempt_discarded"{eprintln!("\n[Generation discarded; switching provider]");rendered=false;}
                    if let Some(text)=event["text"].as_str(){completed=true;if !json_output{if rendered{println!();}else{println!("{text}");}}}
                    if let Some(error)=event["error"].as_str(){failure=Some(error.to_string());}
                }
            }
        }
        task.await??;
        if let Some(error) = failure {
            anyhow::bail!("{error}");
        }
        ensure!(completed, "stream ended before a result");
        Ok(())
    }
}

#[async_trait::async_trait]
impl layeragent_gateway::mcp::McpBackend for Client {
    async fn invoke(
        &self,
        name: &str,
        args: Value,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<Value> {
        match name {
            "layeragent_sessions" => self.get("/v1/sessions").await,
            "layeragent_run" => {
                let id = self
                    .session(
                        args["session_id"].as_str().map(str::to_string),
                        PathBuf::from(args["workspace"].as_str().unwrap_or(".")),
                    )
                    .await?;
                let prompt = args["prompt"]
                    .as_str()
                    .context("prompt required")?
                    .to_string();
                let model = args["model"].as_str().map(str::to_string);
                let (tx, mut rx) = mpsc::channel(64);
                let client = self.clone();
                let task = tokio::spawn(async move { client.turn(&id, &prompt, model, tx).await });
                let mut job = None;
                let mut result = None;
                loop {
                    tokio::select! {_=cancel.cancelled()=>{if let Some(id)=&job{let _=self.post(&format!("/v1/tasks/{id}/cancel"),json!({})).await;}anyhow::bail!("MCP operation cancelled");},event=rx.recv()=>{let Some(event)=event else{break};if let Some(id)=event["job_id"].as_str(){job=Some(id.to_string());}
                    if event.get("text").is_some(){result=Some(event);}else if let Some(error)=event["error"].as_str(){anyhow::bail!("{error}");}}}
                }
                task.await??;
                result.context("turn ended without a result")
            }
            _ => anyhow::bail!("unknown MCP tool"),
        }
    }
}

async fn approval_key(review: &str) -> Result<bool> {
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            let _ = crossterm::terminal::disable_raw_mode();
        }
    }
    use std::io::Write;
    let (width, height) = crossterm::terminal::size()?;
    if width < 36 || height < 12 {
        eprintln!(
            "Approval denied: terminal too small to review; enlarge to at least 36x12 and retry."
        );
        return Ok(false);
    }
    let rows = layeragent_ui::view::wrap(review, width.saturating_sub(2) as usize);
    let viewport = height.saturating_sub(5) as usize;
    let mut scroll = 0usize;
    let mut reviewed = 0usize;
    crossterm::terminal::enable_raw_mode()?;
    let _restore = Restore;
    let mut events = crossterm::event::EventStream::new();
    loop {
        let end = scroll.saturating_add(viewport).min(rows.len());
        if scroll <= reviewed {
            reviewed = reviewed.max(end);
        }
        let mut stderr = std::io::stderr();
        crossterm::execute!(
            stderr,
            crossterm::terminal::Clear(crossterm::terminal::ClearType::All),
            crossterm::cursor::MoveTo(0, 0)
        )?;
        write!(
            stderr,
            "Approval: full action, destination, context, hash and expiry\r\n"
        )?;
        for row in &rows[scroll..end] {
            write!(stderr, "{row}\r\n")?;
        }
        write!(
            stderr,
            "Lines {}-{}/{}; {} remaining; {} reviewed\r\n",
            scroll + 1,
            end,
            rows.len(),
            rows.len().saturating_sub(end),
            reviewed
        )?;
        write!(stderr, "Up/Down PgUp/PgDn Home/End; [N]/Esc deny\r\n")?;
        write!(
            stderr,
            "{}\r\n",
            if reviewed >= rows.len() {
                "[S]/[Y] approve exactly this action"
            } else {
                "Approval blocked: review all lines first"
            }
        )?;
        stderr.flush()?;
        let Some(event) = events.next().await else {
            return Ok(false);
        };
        let event = event?;
        if let crossterm::event::Event::Key(key) = event {
            if key.kind == crossterm::event::KeyEventKind::Release {
                continue;
            }
            use crossterm::event::KeyCode;
            match key.code {
                KeyCode::Char('y' | 'Y' | 's' | 'S') if reviewed >= rows.len() => return Ok(true),
                KeyCode::Char('n' | 'N') | KeyCode::Esc => return Ok(false),
                KeyCode::Char('c')
                    if key
                        .modifiers
                        .contains(crossterm::event::KeyModifiers::CONTROL) =>
                {
                    return Ok(false);
                }
                KeyCode::Down => {
                    scroll = scroll
                        .saturating_add(1)
                        .min(rows.len().saturating_sub(viewport))
                }
                KeyCode::Up => scroll = scroll.saturating_sub(1),
                KeyCode::PageDown => {
                    scroll = scroll
                        .saturating_add(viewport)
                        .min(rows.len().saturating_sub(viewport))
                }
                KeyCode::PageUp => scroll = scroll.saturating_sub(viewport),
                KeyCode::Home => scroll = 0,
                KeyCode::End => scroll = rows.len().saturating_sub(viewport),
                _ => {}
            }
        } else if matches!(event, crossterm::event::Event::Resize(_, _)) {
            // Re-review with current dimensions rather than confirming hidden content.
            return Ok(false);
        }
    }
}
