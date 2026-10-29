//! Terminal front end: owns the terminal, feeds keys and runtime events to the
//! `layeragent-ui` state machine and redraws it on a steady animation clock.
use crate::client::Client;
use anyhow::Result;
use crossterm::event::{
    DisableBracketedPaste, EnableBracketedPaste, Event, EventStream, KeyCode, KeyEvent,
    KeyEventKind, KeyModifiers,
};
use futures_util::StreamExt;
use layeragent_ui::{
    app::{Action, App, Info, Key},
    raster, theme, view,
};
use serde_json::{Value, json};
use std::{path::PathBuf, time::Duration};
use tokio::sync::mpsc;

pub async fn run(client: Client, session: String, workspace: PathBuf) -> Result<()> {
    let forced = std::env::var("LAYERAGENT_COLORS").unwrap_or_default();
    theme::set_truecolor(
        forced == "truecolor" || (forced != "256" && raster::truecolor_supported()),
    );
    let info = system_info(&client, &workspace).await;
    let mut terminal = ratatui::init();
    let _ = crossterm::execute!(std::io::stdout(), EnableBracketedPaste);
    let result = interface(&mut terminal, client, session, workspace, info).await;
    let _ = crossterm::execute!(std::io::stdout(), DisableBracketedPaste);
    ratatui::restore();
    result
}

async fn system_info(client: &Client, workspace: &std::path::Path) -> Info {
    let model = client
        .get("/v1/config")
        .await
        .ok()
        .and_then(|v| v["config"]["primary"].as_str().map(str::to_string))
        .unwrap_or_else(|| "sin configurar".into());
    let path = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());
    let os = std::fs::read_to_string("/etc/os-release")
        .ok()
        .and_then(|t| {
            t.lines().find_map(|l| {
                l.strip_prefix("PRETTY_NAME=")
                    .map(|v| v.trim_matches('"').to_string())
            })
        })
        .unwrap_or_else(|| std::env::consts::OS.to_string());
    let shell = std::env::var("SHELL")
        .ok()
        .and_then(|s| s.rsplit('/').next().map(str::to_string))
        .unwrap_or_else(|| "sh".into());
    Info {
        model,
        project: path
            .file_name()
            .map_or_else(|| "proyecto".into(), |n| n.to_string_lossy().into_owned()),
        path: path.to_string_lossy().into_owned(),
        os,
        shell,
        version: env!("CARGO_PKG_VERSION").to_string(),
    }
}

fn translate(key: KeyEvent) -> Option<Key> {
    if key.kind == KeyEventKind::Release {
        return None;
    }
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    Some(match key.code {
        KeyCode::Char(c) if ctrl => Key::Ctrl(c.to_ascii_lowercase()),
        KeyCode::Char(c) => Key::Char(c),
        KeyCode::Enter => Key::Enter,
        KeyCode::Esc => Key::Esc,
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Delete => Key::Delete,
        KeyCode::Left => Key::Left,
        KeyCode::Right => Key::Right,
        KeyCode::Up => Key::Up,
        KeyCode::Down => Key::Down,
        KeyCode::Home => Key::Home,
        KeyCode::End => Key::End,
        KeyCode::PageUp => Key::PageUp,
        KeyCode::PageDown => Key::PageDown,
        KeyCode::Tab => Key::Tab,
        _ => return None,
    })
}

async fn interface(
    terminal: &mut ratatui::DefaultTerminal,
    client: Client,
    mut session: String,
    workspace: PathBuf,
    info: Info,
) -> Result<()> {
    let mut app = App::new(info);
    match client.get(&format!("/v1/sessions/{session}/history")).await {
        Ok(history) => {
            if let Some(messages) = history.as_array() {
                app.load_history(messages);
            }
        }
        Err(error) => app.note(&format!("No se pudo cargar el historial: {error}")),
    }
    let mut events = EventStream::new();
    let (tx, mut rx) = mpsc::channel::<(u64, Value)>(128);
    let mut view_generation = 0u64;
    let mut follower: Option<tokio::task::JoinHandle<()>> = None;
    let start = std::time::Instant::now();
    let mut tick = tokio::time::interval(Duration::from_millis(66));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut job: Option<String> = None;
    let mut cancel_on_start = false;
    app.tick(0.0);
    loop {
        terminal.draw(|frame| view::draw(&mut app, frame))?;
        if app.finished() {
            break;
        }
        let mut action = Action::None;
        tokio::select! {
            _ = tick.tick() => app.tick(start.elapsed().as_secs_f32()),
            event = events.next() => match event {
                Some(Ok(Event::Key(key))) => {
                    if let Some(key) = translate(key) {
                        action = app.on_key(key);
                    }
                }
                Some(Ok(Event::Paste(text))) => app.on_paste(&text),
                Some(Ok(_)) => {}
                _ => break,
            },
            Some((generation, event)) = rx.recv() => {
                if generation != view_generation { continue; }
                if let Some(id) = event["job_id"].as_str() {
                    job = Some(id.to_string());
                    if cancel_on_start {
                        let _ = client.post(&format!("/v1/tasks/{id}/cancel"), json!({})).await;
                    }
                }
                app.on_event(&event);
                if !app.is_busy() {
                    if app.team.is_none() { job = None; }
                    cancel_on_start = false;
                }
            }
        }
        match action {
            Action::None => {}
            Action::Context => match client.get(&format!("/v1/sessions/{session}/context")).await {
                Ok(value) => app.note(&format!("Contexto estimado: {} tokens; ventana: {}. Historial completo conservado. Checkpoint: {}", value["estimated_tokens"], value["model_context_window"], value["checkpoint"]["kind"])),
                Err(error) => app.note(&format!("No se pudo consultar el contexto: {error}")),
            },
            Action::Compact => match client.post(&format!("/v1/sessions/{session}/compact"), json!({})).await {
                Ok(value) => app.note(&format!("Checkpoint {} guardado hasta el mensaje {}. Historial completo conservado.", value["kind"], value["through_seq"])),
                Err(error) => app.note(&format!("No se pudo compactar: {error}")),
            },
            Action::Effort(level) => {
                let path = format!("/v1/sessions/{session}/preferences");
                let result = if let Some(level) = level { client.post(&path, json!({"effort":level,"bridge":false})).await } else { client.get(&path).await };
                match result {
                    Ok(value) => app.note(&format!("Esfuerzo de sesión: {}. Se aplica entre operaciones, salvo override de tarea.", value["effort"])),
                    Err(error) => app.note(&format!("No se pudo consultar o cambiar el esfuerzo: {error}")),
                }
            }
            Action::Send(prompt) => {
                cancel_on_start = false;
                job = None;
                if let Some(handle) = follower.take() { handle.abort(); }
                view_generation += 1;
                let options = crate::team::ExecutionOptions { execution: app.execution.clone(), ..Default::default() }.body()?;
                follower = Some(spawn_follow(client.clone(), session.clone(), Some((prompt, options)), None, tx.clone(), view_generation));
            }
            Action::Team(command) => {
                if let Some(start) = command.strip_prefix("start ") {
                    match crate::team::parse_start(start) {
                        Ok((options, objective)) => {
                            if let Some(handle) = follower.take() { handle.abort(); }
                            view_generation += 1;
                            job = None;
                            cancel_on_start = false;
                            app.begin_team(&objective);
                            follower = Some(spawn_follow(client.clone(), session.clone(), Some((objective, options.body()?)), None, tx.clone(), view_generation));
                        }
                        Err(error) => app.note(&format!("No se pudo iniciar el equipo: {error}")),
                    }
                } else if let Some(task) = command.strip_prefix("attach ") {
                    let task = task.trim();
                    let result = async {
                        crate::team::identifier(task)?;
                        let value = client.get(&format!("/v1/tasks/{task}")).await?;
                        anyhow::ensure!(value["execution"] == "cooperative", "la tarea no es cooperativa");
                        anyhow::ensure!(value["project"].as_str() == Some(workspace.canonicalize()?.to_string_lossy().as_ref()), "el equipo pertenece a otro proyecto; abre su espacio de trabajo");
                        Ok::<_, anyhow::Error>(value)
                    }.await;
                    match result {
                        Ok(value) => {
                            if let Some(handle) = follower.take() { handle.abort(); }
                            view_generation += 1;
                            job = Some(task.into());
                            if let Some(id) = value["session_id"].as_str() { session = id.into(); }
                            cancel_on_start = false;
                            follower = Some(spawn_follow(client.clone(), session.clone(), None, Some(task.into()), tx.clone(), view_generation));
                        }
                        Err(error) => app.note(&format!("No se pudo consultar el equipo: {error}")),
                    }
                } else if let Some(task) = app.team.as_ref().and_then(|view| view["team"]["task_id"].as_str()) {
                    match team_action(&client, task, &command).await {
                        Ok(value) => app.team_note(&format!("Equipo: {}", crate::team::terminal_text(&value))),
                        Err(error) => app.team_note(&format!("No se aplicó la orden: {error}. La vista se actualizará; un conflicto requiere repetir la orden explícitamente.")),
                    }
                } else {
                    app.note("No hay un equipo seleccionado. Usa /team start o /team attach <task-id>.");
                }
            }
            Action::Cancel => match &job {
                Some(id) => {
                    let _ = client
                        .post(&format!("/v1/tasks/{id}/cancel"), json!({}))
                        .await;
                }
                None => cancel_on_start = true,
            },
            Action::Approve {
                id,
                allow,
                expected_revision,
                arguments_hash,
            } => {
                if let Err(e) = client
                    .post(
                        &format!("/v1/approvals/{id}"),
                        json!({
                            "allow": allow,
                            "expected_revision": expected_revision,
                            "arguments_hash": arguments_hash,
                        }),
                    )
                    .await
                {
                    app.note(&format!("No se pudo responder a la aprobación: {e}"));
                }
            }
            Action::NewSession => match client.session(None, workspace.clone()).await {
                Ok(fresh) => {
                    session = fresh;
                    job = None;
                    cancel_on_start = false;
                    if let Some(handle) = follower.take() { handle.abort(); }
                    view_generation += 1;
                    app.reset_conversation();
                }
                Err(e) => app.note(&format!("No se pudo crear la sesión: {e}")),
            },
        }
    }
    if let Some(handle) = follower {
        handle.abort();
    }
    Ok(())
}

fn spawn_follow(
    client: Client,
    session: String,
    new: Option<(String, Value)>,
    existing: Option<String>,
    output: mpsc::Sender<(u64, Value)>,
    generation: u64,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let (tx, mut rx) = mpsc::channel(128);
        let run = async {
            let task = if let Some((prompt, options)) = new {
                client.submit_turn(&session, &prompt, None, options).await?
            } else {
                let id = existing
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("task missing"))?;
                client.get(&format!("/v1/tasks/{id}")).await?
            };
            let id = task["id"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("task id missing"))?;
            tx.send(json!({"job_id":id,"task_id":id,"execution":task["execution"]}))
                .await?;
            if task["execution"] == "cooperative" {
                client.follow_team(id, tx.clone(), false).await
            } else {
                client.follow_task(id, &session, tx.clone()).await
            }
        };
        tokio::pin!(run);
        loop {
            tokio::select! {
                result = &mut run => {
                    while let Ok(event) = rx.try_recv() { let _ = output.send((generation,event)).await; }
                    if let Err(error) = result { let _ = output.send((generation,json!({"error":error.to_string()}))).await; }
                    break;
                }
                Some(event) = rx.recv() => { if output.send((generation,event)).await.is_err() { break; } }
            }
        }
    })
}

async fn team_action(client: &Client, task: &str, text: &str) -> Result<String> {
    let (action, rest) = text.split_once(' ').unwrap_or((text, ""));
    let value = match action {
        "show" => {
            return Ok(crate::team::summary(
                &client.get(&format!("/v1/tasks/{task}/team")).await?,
            ));
        }
        "message" => {
            let (recipient, body) = rest
                .split_once(' ')
                .ok_or_else(|| anyhow::anyhow!("Uso: /team message <miembro> <texto>"))?;
            crate::team::message(client, task, recipient, body).await?
        }
        "pause" | "resume" | "close" | "cancel" => {
            let reason = rest.strip_prefix("--reason ").unwrap_or(rest).trim();
            crate::team::command(
                client,
                task,
                action,
                None,
                None,
                (!reason.is_empty()).then_some(reason),
            )
            .await?
        }
        "priority" => {
            let (work, priority) = rest
                .split_once(' ')
                .ok_or_else(|| anyhow::anyhow!("Uso: /team priority <work-id> <valor>"))?;
            crate::team::command(
                client,
                task,
                action,
                Some(work),
                Some(priority.trim().parse()?),
                None,
            )
            .await?
        }
        "export" => {
            anyhow::bail!(
                "Exportación a stdout: layeragent team export {task} --format json|markdown"
            )
        }
        _ => anyhow::bail!(
            "Uso: /team start|attach|show|message|pause|resume|close|cancel|priority|toggle"
        ),
    };
    Ok(serde_json::to_string_pretty(&value)?)
}
