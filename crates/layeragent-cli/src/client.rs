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

#[derive(Debug)]
struct ApiStatus {
    status: reqwest::StatusCode,
    retry_after: u64,
    message: String,
}
impl std::fmt::Display for ApiStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "API request failed ({}): {}", self.status, self.message)
    }
}
impl std::error::Error for ApiStatus {}

async fn json_response(response: reqwest::Response) -> Result<Value> {
    let status = response.status();
    let retry_after = response
        .headers()
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse().ok())
        .unwrap_or(3);
    if !status.is_success() {
        let body: Value = response.json().await.unwrap_or(Value::Null);
        return Err(ApiStatus {
            status,
            retry_after,
            message: body["error"].as_str().unwrap_or("request rejected").into(),
        }
        .into());
    }
    Ok(response.json().await?)
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
        json_response(response).await
    }
    pub async fn post(&self, path: &str, body: Value) -> Result<Value> {
        let response = self
            .http
            .post(format!("{}{path}", self.base))
            .json(&body)
            .send()
            .await?;
        json_response(response).await
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
        self.turn_options(session, prompt, model, json!({}), tx)
            .await
    }

    pub async fn submit_turn(
        &self,
        session: &str,
        prompt: &str,
        model: Option<String>,
        mut options: Value,
    ) -> Result<Value> {
        let body = options
            .as_object_mut()
            .context("task options must be an object")?;
        body.insert("session_id".into(), json!(session));
        body.insert("objective".into(), json!(prompt));
        body.insert("model".into(), json!(model));
        let key = layeragent_protocol::new_id();
        let response = self
            .http
            .post(format!("{}/v1/tasks", self.base))
            .header("idempotency-key", key)
            .json(&options)
            .send()
            .await?;
        let status = response.status();
        let value: Value = response.json().await?;
        ensure!(
            status.is_success(),
            "{}",
            value["error"].as_str().unwrap_or("task rejected")
        );
        Ok(value)
    }

    pub async fn turn_options(
        &self,
        session: &str,
        prompt: &str,
        model: Option<String>,
        options: Value,
        tx: mpsc::Sender<Value>,
    ) -> Result<()> {
        let created = self.submit_turn(session, prompt, model, options).await?;
        let task = created["id"].as_str().context("task response has no id")?;
        if tx.send(json!({"job_id":task,"task_id":task,"session_id":session,"execution":created["execution"]})).await.is_err() { return Ok(()); }
        if created["execution"] == "cooperative" {
            self.follow_team(task, tx, true).await
        } else {
            self.follow_task(task, session, tx).await
        }
    }

    pub async fn follow_task(
        &self,
        task: &str,
        session: &str,
        tx: mpsc::Sender<Value>,
    ) -> Result<()> {
        crate::team::identifier(task)?;
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
    pub async fn team_resource(&self, task: &str, resource: &str) -> Result<Value> {
        crate::team::identifier(task)?;
        ensure!(
            matches!(
                resource,
                "messages"
                    | "deliveries"
                    | "work"
                    | "findings"
                    | "finding_revisions"
                    | "objections"
                    | "evidence"
            ),
            "unknown team resource"
        );
        let mut cursor = 0;
        let mut items = Vec::new();
        loop {
            let page = self
                .get(&format!(
                    "/v1/tasks/{task}/team/{resource}?after={cursor}&limit=200"
                ))
                .await?;
            items.extend(
                page["items"]
                    .as_array()
                    .context("resource items missing")?
                    .iter()
                    .cloned(),
            );
            match page["next"].as_i64() {
                Some(next) => {
                    ensure!(next > cursor, "resource cursor did not advance");
                    cursor = next;
                }
                None => return Ok(json!({"items":items,"next":null})),
            }
        }
    }

    async fn team_view(&self, task: &str, mut snapshot: Value) -> Result<Value> {
        // One request rather than a request per resource. Export retains complete
        // data; only the terminal projection is limited to the most recent rows.
        let mut export = self.get(&format!("/v1/tasks/{task}/team/export")).await?;
        for resource in [
            "messages",
            "deliveries",
            "work",
            "findings",
            "objections",
            "evidence",
            "finding_revisions",
        ] {
            let mut items = export[resource].take();
            if let Some(rows) = items.as_array_mut() {
                let excess = rows.len().saturating_sub(200);
                rows.drain(..excess);
            }
            snapshot[resource] = items;
        }
        Ok(snapshot)
    }

    /// Snapshot first, then the durable SSE cursor. Reconnect never writes or resumes.
    /// View refresh is event driven and throttled independently from the event stream.
    pub async fn follow_team(
        &self,
        task: &str,
        tx: mpsc::Sender<Value>,
        stop_when_blocked: bool,
    ) -> Result<()> {
        crate::team::identifier(task)?;
        let mut cursor = None;
        let mut failures = 0u32;
        let mut approvals = std::collections::HashSet::new();
        loop {
            if tx.is_closed() {
                return Ok(());
            }
            let connection: Result<bool> = async {
                let snapshot = self.get(&format!("/v1/tasks/{task}/team")).await?;
                let boundary = snapshot["team"]["last_event_seq"].as_i64().unwrap_or(0);
                let after = *cursor.get_or_insert(boundary);
                if self.send_team_view(task, snapshot, &tx, &mut approvals, stop_when_blocked).await? { return Ok(true); }
                let response = self.http.get(format!("{}/v1/tasks/{task}/events/stream?after={after}",self.base)).send().await?;
                if !response.status().is_success() { json_response(response).await?; unreachable!(); }
                let mut stream = response.bytes_stream();
                let mut decoder = layeragent_providers::FrameDecoder::default();
                let mut tick = tokio::time::interval(std::time::Duration::from_secs(3));
                tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                let mut dirty = false;
                loop {
                    tokio::select! {
                        _ = tx.closed() => return Ok(true),
                        _ = tick.tick(), if dirty => {
                            let snapshot = self.get(&format!("/v1/tasks/{task}/team")).await?;
                            if self.send_team_view(task,snapshot,&tx,&mut approvals,stop_when_blocked).await? { return Ok(true); }
                            dirty = false;
                        }
                        chunk = stream.next() => {
                            let Some(chunk) = chunk else { return Ok(false); };
                            for frame in decoder.push(&chunk?,false)? {
                                if frame.trim().is_empty() { continue; }
                                let event:Value = serde_json::from_str(&frame)?;
                                let seq = event["seq"].as_i64().context("event sequence missing")?;
                                if seq <= cursor.unwrap_or(0) { continue; }
                                cursor = Some(seq);
                                dirty = true;
                                if tx.send(event).await.is_err() { return Ok(true); }
                            }
                            failures = 0;
                        }
                    }
                }
            }.await;
            match connection {
                Ok(true) => return Ok(()),
                Ok(false) => {
                    failures += 1;
                }
                Err(error) => {
                    failures += 1;
                    let status = error.downcast_ref::<ApiStatus>();
                    if status.is_some_and(|e| {
                        e.status.is_client_error()
                            && e.status != reqwest::StatusCode::TOO_MANY_REQUESTS
                    }) || failures >= 20
                    {
                        return Err(error);
                    }
                    if failures == 1 {
                        let _ = tx.send(json!({"kind":"team_connection","data":{"state":"reconnecting","reason":"Conservando el cursor; no se reanuda ni repite trabajo."}})).await;
                    }
                    if let Some(status) = status {
                        tokio::time::sleep(std::time::Duration::from_secs(
                            status.retry_after.clamp(1, 30),
                        ))
                        .await;
                    }
                }
            }
            ensure!(failures < 20, "team stream did not reconnect");
            tokio::time::sleep(std::time::Duration::from_millis(
                500 * u64::from(failures.min(10)),
            ))
            .await;
        }
    }

    async fn send_team_view(
        &self,
        task: &str,
        snapshot: Value,
        tx: &mpsc::Sender<Value>,
        approvals: &mut std::collections::HashSet<String>,
        stop_when_blocked: bool,
    ) -> Result<bool> {
        let view = self.team_view(task, snapshot.clone()).await?;
        if tx
            .send(json!({"kind":"team_snapshot","task_id":task,"data":view}))
            .await
            .is_err()
        {
            return Ok(true);
        }
        if let Some(pending) = snapshot["team"]["pending_approvals"].as_array() {
            for approval in pending {
                if let Some(id) = approval["approval_id"].as_str()
                    && approvals.insert(id.to_owned())
                    && tx
                        .send(json!({"kind":"approval_required","task_id":task,"data":approval}))
                        .await
                        .is_err()
                {
                    return Ok(true);
                }
            }
        }
        let state = snapshot["team"]["state"].as_str().unwrap_or_default();
        if matches!(state, "terminated" | "cancelled" | "failed")
            || (stop_when_blocked && state == "blocked")
        {
            let state = if state == "terminated" {
                "completed"
            } else {
                state
            };
            let _ = tx.send(json!({"kind":"task_outcome","task_id":task,"data":{"state":state,"reason":snapshot["team"]["reason"]}})).await;
            return Ok(true);
        }
        Ok(false)
    }

    pub async fn print_turn(
        &self,
        session: &str,
        prompt: &str,
        model: Option<String>,
        json_output: bool,
    ) -> Result<()> {
        self.print_turn_options(session, prompt, model, json_output, json!({}))
            .await
    }
    pub async fn print_turn_options(
        &self,
        session: &str,
        prompt: &str,
        model: Option<String>,
        json_output: bool,
        options: Value,
    ) -> Result<()> {
        use std::io::{IsTerminal, Write};
        let (tx, mut rx) = mpsc::channel(64);
        let client = self.clone();
        let id = session.to_string();
        let prompt = prompt.to_string();
        let task =
            tokio::spawn(
                async move { client.turn_options(&id, &prompt, model, options, tx).await },
            );
        let mut job: Option<String> = None;
        let mut failure = None;
        let mut completed = false;
        let mut rendered = false;
        let mut cancelled = false;
        let mut seen_approvals = std::collections::HashSet::new();
        eprintln!("Session: {session}");
        loop {
            tokio::select! {
                _=tokio::signal::ctrl_c()=>{
                    cancelled=true;
                    if let Some(id)=&job{let _=self.post(&format!("/v1/tasks/{id}/cancel"),json!({})).await;}
                },
                event=rx.recv()=>{
                    let Some(mut event)=event else{break};
                    if json_output{println!("{event}");}
                    if event["kind"] == "team_member_event" && event["data"]["kind"] == "approval_required" {
                        event = json!({"kind":"approval_required","data":event["data"]["data"]});
                    }
                    if let Some(id)=event["job_id"].as_str(){job=Some(id.to_string());if cancelled{let _=self.post(&format!("/v1/tasks/{id}/cancel"),json!({})).await;}}
                    if event["kind"]=="approval_required"{
                        let id=event["data"]["approval_id"].as_str().unwrap_or("");
                        if !seen_approvals.insert(id.to_owned()) { continue; }
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
                    if event["kind"] == "team_snapshot" && !json_output { eprint!("{}", crate::team::summary(&event["data"])); }
                    if event["kind"] == "team_completed" { completed = true; }
                    if event["kind"] == "task_outcome" {
                        completed = event["data"]["state"] == "completed";
                        if !completed { failure = Some(format!("Equipo {}: {}", event["data"]["state"].as_str().unwrap_or("detenido"), event["data"]["reason"].as_str().unwrap_or("consulta team show para continuar"))); }
                        if !json_output { println!("Equipo: {}", event["data"]["state"]); }
                    }
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

#[cfg(test)]
mod team_tests {
    use super::*;
    use axum::{
        Json,
        extract::State,
        http::{Method, StatusCode},
        response::IntoResponse,
    };
    use std::sync::{Arc, Mutex};

    type Calls = Arc<Mutex<Vec<(String, String, Value)>>>;
    async fn fixture(
        State(calls): State<Calls>,
        request: axum::extract::Request,
    ) -> axum::response::Response {
        let method = request.method().clone();
        let uri = request.uri().to_string();
        let bytes = match axum::body::to_bytes(request.into_body(), 65536).await {
            Ok(bytes) => bytes,
            Err(_) => return StatusCode::BAD_REQUEST.into_response(),
        };
        let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        if let Ok(mut calls) = calls.lock() {
            calls.push((method.to_string(), uri.clone(), body));
        }
        if method == Method::POST {
            return (
                StatusCode::CONFLICT,
                Json(json!({"error":"team revision conflict"})),
            )
                .into_response();
        }
        if uri.ends_with("/team") {
            let first_live = uri.contains("task-live")
                && calls
                    .lock()
                    .map(|calls| {
                        calls
                            .iter()
                            .filter(|call| call.1.ends_with("/team"))
                            .count()
                            == 1
                    })
                    .unwrap_or(false);
            return Json(json!({"team":{"task_id":"task-one","state":if first_live {"active"} else {"terminated"},"revision":17,"last_event_seq":5,"members":[]},"task_revision":999})).into_response();
        }
        if uri.ends_with("/team/export") {
            return Json(json!({"messages":[{"id":"first"},{"id":"second"}],"work":[],"findings":[],"evidence":[],"objections":[],"deliveries":[],"finding_revisions":[]})).into_response();
        }
        if uri.contains("/events/stream?") {
            let data = json!({"seq":6,"kind":"team_message","data":{}});
            return (
                [("content-type", "text/event-stream")],
                format!("data: {data}\n\ndata: {data}\n\n"),
            )
                .into_response();
        }
        if uri.contains("/events?") {
            return Json(json!({"events":[{"seq":6,"kind":"team_completed","data":{}}],"next":6}))
                .into_response();
        }
        if uri.contains("/messages?after=0") {
            return Json(json!({"items":[{"id":"first"}],"next":7})).into_response();
        }
        if uri.contains("/messages?after=7") {
            return Json(json!({"items":[{"id":"second"}],"next":null})).into_response();
        }
        Json(json!({"items":[],"next":null})).into_response()
    }
    async fn server() -> Result<(Client, Calls, tokio::task::JoinHandle<()>)> {
        let calls = Calls::default();
        let app = axum::Router::new()
            .fallback(fixture)
            .with_state(calls.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let base = format!("http://{}", listener.local_addr()?);
        let handle = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Ok((
            Client {
                http: reqwest::Client::new(),
                base,
            },
            calls,
            handle,
        ))
    }
    #[tokio::test]
    async fn attaching_a_completed_team_only_reads_and_returns_an_outcome() -> Result<()> {
        let (client, calls, server) = server().await?;
        let (tx, mut rx) = mpsc::channel(16);
        client.follow_team("task-one", tx, false).await?;
        let first = rx.recv().await.context("snapshot missing")?;
        assert_eq!(first["kind"], "team_snapshot");
        assert_eq!(first["data"]["messages"].as_array().map(Vec::len), Some(2));
        let mut last = Value::Null;
        while let Some(event) = rx.recv().await {
            last = event;
        }
        assert_eq!(last["kind"], "task_outcome");
        assert_eq!(last["data"]["state"], "completed");
        let calls = calls
            .lock()
            .map_err(|_| anyhow::anyhow!("calls poisoned"))?;
        assert!(calls.iter().all(|call| call.0 == "GET"));
        assert!(
            !calls.iter().any(|call| call.1.contains("events")),
            "terminal snapshot needs no live stream"
        );
        server.abort();
        Ok(())
    }
    #[tokio::test]
    async fn conflicting_commands_use_team_revision_and_are_not_replayed() -> Result<()> {
        let (client, calls, server) = server().await?;
        assert!(
            crate::team::command(&client, "task-one", "pause", None, None, None)
                .await
                .is_err()
        );
        let calls = calls
            .lock()
            .map_err(|_| anyhow::anyhow!("calls poisoned"))?;
        let writes: Vec<_> = calls.iter().filter(|call| call.0 == "POST").collect();
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].2["expected_revision"], 17);
        assert!(
            !writes[0].2["idempotency_key"]
                .as_str()
                .unwrap_or_default()
                .is_empty()
        );
        server.abort();
        Ok(())
    }

    #[tokio::test]
    async fn stream_starts_at_snapshot_cursor_and_deduplicates_replayed_events() -> Result<()> {
        let (client, calls, server) = server().await?;
        let (tx, mut rx) = mpsc::channel(16);
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            client.follow_team("task-live", tx, false),
        )
        .await??;
        let mut messages = 0;
        while let Some(event) = rx.recv().await {
            if event["kind"] == "team_message" {
                messages += 1;
            }
        }
        assert_eq!(messages, 1);
        assert!(
            calls
                .lock()
                .map_err(|_| anyhow::anyhow!("calls poisoned"))?
                .iter()
                .any(|call| call.1.contains("events/stream?after=5"))
        );
        let page = client.team_resource("task-one", "messages").await?;
        assert_eq!(page["items"].as_array().map(Vec::len), Some(2));
        server.abort();
        Ok(())
    }
}
