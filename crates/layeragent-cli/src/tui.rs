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
    let mut events = EventStream::new();
    let (tx, mut rx) = mpsc::channel::<Value>(128);
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
            Some(event) = rx.recv() => {
                if let Some(id) = event["job_id"].as_str() {
                    job = Some(id.to_string());
                    if cancel_on_start {
                        let _ = client.post(&format!("/v1/tasks/{id}/cancel"), json!({})).await;
                    }
                }
                app.on_event(&event);
                if !app.is_busy() {
                    job = None;
                    cancel_on_start = false;
                }
            }
        }
        match action {
            Action::None => {}
            Action::Send(prompt) => {
                cancel_on_start = false;
                let (c, s, t) = (client.clone(), session.clone(), tx.clone());
                tokio::spawn(async move {
                    if let Err(e) = c.turn(&s, &prompt, None, t.clone()).await {
                        let _ = t.send(json!({"error": e.to_string()})).await;
                    }
                });
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
                    app.reset_conversation();
                }
                Err(e) => app.note(&format!("No se pudo crear la sesión: {e}")),
            },
        }
    }
    Ok(())
}
