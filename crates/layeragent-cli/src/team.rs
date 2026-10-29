//! Terminal access to the durable team API. Watching never resumes execution.
use crate::client::Client;
use anyhow::{Context, Result, ensure};
use clap::{Args, Parser, Subcommand};
use layeragent_protocol::{CooperativeOptions, ExecutionMode, TeamTemplate};
use serde_json::{Value, json};

#[derive(Args, Clone, Debug, Default)]
pub struct ExecutionOptions {
    #[arg(long, default_value = "delegated", value_parser = ["individual", "delegated", "cooperative"])]
    pub execution: String,
    #[arg(long, value_parser = ["audit", "debug", "review"])]
    pub template: Option<String>,
    #[arg(long)]
    pub max_tokens: Option<u64>,
    #[arg(long)]
    pub max_seconds: Option<u64>,
    #[arg(long)]
    pub max_cost_usd: Option<f64>,
}
impl ExecutionOptions {
    pub fn body(&self) -> Result<Value> {
        let mode: ExecutionMode = if self.execution.is_empty() {
            "delegated"
        } else {
            &self.execution
        }
        .parse()?;
        let configured = self.template.is_some()
            || self.max_tokens.is_some()
            || self.max_seconds.is_some()
            || self.max_cost_usd.is_some();
        ensure!(
            mode == ExecutionMode::Cooperative || !configured,
            "team options require --execution cooperative"
        );
        if mode != ExecutionMode::Cooperative {
            return Ok(json!({"execution":mode}));
        }
        let mut options = CooperativeOptions {
            template: self
                .template
                .as_deref()
                .unwrap_or("audit")
                .parse::<TeamTemplate>()?,
            ..Default::default()
        };
        if let Some(value) = self.max_tokens {
            options.limits.max_tokens = value;
        }
        if let Some(value) = self.max_seconds {
            options.limits.max_active_seconds = value;
        }
        if let Some(value) = self.max_cost_usd {
            options.limits.max_cost_usd = Some(value);
        }
        let options = options.normalized(None)?;
        Ok(json!({"execution":mode,"cooperative":options}))
    }
}

#[derive(Subcommand, Debug)]
pub enum TeamCommand {
    Show(Inspect),
    Watch(Inspect),
    Messages(Inspect),
    Work(Inspect),
    Findings(Inspect),
    Objections(Inspect),
    Evidence(Inspect),
    Message {
        task: String,
        #[arg(long)]
        to: String,
        text: String,
    },
    Pause(Control),
    Resume(Control),
    Close(Control),
    Cancel(Control),
    Priority {
        task: String,
        work: String,
        value: i64,
    },
    Export {
        task: String,
        #[arg(long, default_value = "markdown", value_parser = ["json", "markdown"])]
        format: String,
    },
}
#[derive(Args, Debug)]
pub struct Inspect {
    pub task: String,
    #[arg(long)]
    pub json: bool,
}
#[derive(Args, Debug)]
pub struct Control {
    pub task: String,
    #[arg(long)]
    pub reason: Option<String>,
}

pub fn identifier(id: &str) -> Result<()> {
    ensure!(
        !id.is_empty()
            && id.len() <= 200
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
        "invalid identifier"
    );
    Ok(())
}

pub fn recipient(snapshot: &Value, name: &str) -> Result<String> {
    let members = snapshot["team"]["members"]
        .as_array()
        .context("team members missing")?;
    let matches: Vec<_> = members
        .iter()
        .filter(|m| m["id"] == name || m["name"] == name)
        .collect();
    ensure!(
        matches.len() == 1,
        "recipient must match exactly one member id or name"
    );
    Ok(matches[0]["id"]
        .as_str()
        .context("member id missing")?
        .into())
}

pub async fn command(
    client: &Client,
    task: &str,
    action: &str,
    recipient: Option<&str>,
    priority: Option<i64>,
    reason: Option<&str>,
) -> Result<Value> {
    identifier(task)?;
    let snapshot = client.get(&format!("/v1/tasks/{task}/team")).await?;
    client
        .post(
            &format!("/v1/tasks/{task}/team/commands/{action}"),
            json!({
                "idempotency_key":layeragent_protocol::new_id(),
                "expected_revision":snapshot["team"]["revision"],
                "recipient":recipient,"priority":priority,"reason":reason,
            }),
        )
        .await
}

pub async fn message(client: &Client, task: &str, to: &str, text: &str) -> Result<Value> {
    identifier(task)?;
    let snapshot = client.get(&format!("/v1/tasks/{task}/team")).await?;
    let to = recipient(&snapshot, to)?;
    client
        .post(
            &format!("/v1/tasks/{task}/team/messages"),
            json!({
                "idempotency_key":layeragent_protocol::new_id(),
                "expected_revision":snapshot["team"]["revision"],
                "recipient":to,"purpose":"user_instruction","body":text,
            }),
        )
        .await
}

pub fn summary(value: &Value) -> String {
    let team = &value["team"];
    let mut out = format!(
        "Equipo {} · {} · {}\nPreguntas abiertas: {} · Consumo: {} · Reservas: {}\n",
        team["task_id"].as_str().unwrap_or("?"),
        team["state"].as_str().unwrap_or("?"),
        team["template"].as_str().unwrap_or("?"),
        team["open_questions"],
        team["usage"],
        team["reservations"]
    );
    if let Some(reason) = team["reason"].as_str() {
        out.push_str(&format!("Motivo: {reason}\n"));
    }
    if team["pause_pending"] == true {
        out.push_str("Pausa pendiente: las operaciones activas están terminando.\n");
    }
    if let Some(members) = team["members"].as_array() {
        for member in members {
            out.push_str(&format!(
                "{} ({}) · {} · {}{}\n",
                member["name"].as_str().unwrap_or("?"),
                member["id"].as_str().unwrap_or("?"),
                member["role"].as_str().unwrap_or("?"),
                member["state"].as_str().unwrap_or("?"),
                member["reason"]
                    .as_str()
                    .map(|s| format!(" · {s}"))
                    .unwrap_or_default()
            ));
        }
    }
    terminal_text(&out)
}

/// Neutralize terminal escapes and invisible directional overrides in remote text.
pub fn terminal_text(text: &str) -> String {
    text.chars()
        .flat_map(|c| {
            if (c.is_control() && c != '\n' && c != '\t')
                || matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
            {
                c.escape_unicode().collect::<Vec<_>>()
            } else {
                vec![c]
            }
        })
        .collect()
}

pub async fn dispatch(client: &Client, action: TeamCommand) -> Result<()> {
    let output = match action {
        TeamCommand::Show(args) => {
            identifier(&args.task)?;
            let value = client.get(&format!("/v1/tasks/{}/team", args.task)).await?;
            if !args.json {
                print!("{}", summary(&value));
                return Ok(());
            }
            value
        }
        TeamCommand::Watch(args) => return watch(client, &args.task, args.json).await,
        TeamCommand::Messages(args) => client.team_resource(&args.task, "messages").await?,
        TeamCommand::Work(args) => client.team_resource(&args.task, "work").await?,
        TeamCommand::Findings(args) => client.team_resource(&args.task, "findings").await?,
        TeamCommand::Objections(args) => client.team_resource(&args.task, "objections").await?,
        TeamCommand::Evidence(args) => client.team_resource(&args.task, "evidence").await?,
        TeamCommand::Message { task, to, text } => message(client, &task, &to, &text).await?,
        TeamCommand::Pause(args) => {
            command(
                client,
                &args.task,
                "pause",
                None,
                None,
                args.reason.as_deref(),
            )
            .await?
        }
        TeamCommand::Resume(args) => {
            command(
                client,
                &args.task,
                "resume",
                None,
                None,
                args.reason.as_deref(),
            )
            .await?
        }
        TeamCommand::Close(args) => {
            command(
                client,
                &args.task,
                "close",
                None,
                None,
                args.reason.as_deref(),
            )
            .await?
        }
        TeamCommand::Cancel(args) => {
            command(
                client,
                &args.task,
                "cancel",
                None,
                None,
                args.reason.as_deref(),
            )
            .await?
        }
        TeamCommand::Priority { task, work, value } => {
            command(client, &task, "priority", Some(&work), Some(value), None).await?
        }
        TeamCommand::Export { task, format } => {
            identifier(&task)?;
            let value = client.get(&format!("/v1/tasks/{task}/team/export")).await?;
            if format == "markdown" {
                print!(
                    "{}",
                    value["markdown"]
                        .as_str()
                        .context("export markdown missing")?
                );
                return Ok(());
            }
            value
        }
    };
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

async fn watch(client: &Client, task: &str, json_output: bool) -> Result<()> {
    let (tx, mut rx) = tokio::sync::mpsc::channel(128);
    let follow = client.follow_team(task, tx, false);
    tokio::pin!(follow);
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => { eprintln!("Vista desconectada; el equipo continúa."); return Ok(()); }
            result = &mut follow => { result?; while let Ok(event) = rx.try_recv() { print_event(&event, json_output); } return Ok(()); }
            Some(event) = rx.recv() => print_event(&event, json_output),
        }
    }
}
fn print_event(event: &Value, json_output: bool) {
    if json_output {
        println!("{event}");
    } else if event["kind"] == "team_snapshot" {
        print!("{}", summary(&event["data"]));
    } else {
        println!(
            "{} · {}",
            event["kind"].as_str().unwrap_or("event"),
            event["data"]
        );
    }
}

#[derive(Parser)]
struct StartArgs {
    #[arg(value_parser = ["audit", "debug", "review"])]
    template: String,
    #[arg(long)]
    max_tokens: Option<u64>,
    #[arg(long)]
    max_seconds: Option<u64>,
    #[arg(long)]
    max_cost_usd: Option<f64>,
}
pub fn parse_start(text: &str) -> Result<(ExecutionOptions, String)> {
    let (options, objective) = text.split_once(" -- ").context("Uso: /team start audit|debug|review [--max-tokens N] [--max-seconds N] [--max-cost-usd N] -- objetivo")?;
    ensure!(!objective.trim().is_empty(), "objective is required");
    let args =
        StartArgs::try_parse_from(std::iter::once("start").chain(options.split_whitespace()))?;
    let options = ExecutionOptions {
        execution: "cooperative".into(),
        template: Some(args.template),
        max_tokens: args.max_tokens,
        max_seconds: args.max_seconds,
        max_cost_usd: args.max_cost_usd,
    };
    options.body()?;
    Ok((options, objective.trim().into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn start_limits_and_defaults_preserve_the_contract() -> Result<()> {
        let (options, objective) = parse_start(
            "debug --max-tokens 10000 --max-seconds 50 --max-cost-usd 1.5 -- inspect -- strange text",
        )?;
        let body = options.body()?;
        assert_eq!(objective, "inspect -- strange text");
        assert_eq!(body["cooperative"]["limits"]["max_tokens"], 10000);
        assert_eq!(body["cooperative"]["limits"]["max_active_seconds"], 50);
        assert!(parse_start("audit --max-tokens 0 -- x").is_err());
        assert!(
            ExecutionOptions {
                template: Some("audit".into()),
                ..Default::default()
            }
            .body()
            .is_err()
        );
        assert_eq!(
            ExecutionOptions::default().body()?["execution"],
            "delegated"
        );
        Ok(())
    }
    #[test]
    fn names_are_exact_and_ambiguous_names_fail() -> Result<()> {
        let snapshot =
            json!({"team":{"members":[{"id":"a","name":"reviewer"},{"id":"b","name":"reviewer"}]}});
        assert_eq!(recipient(&snapshot, "a")?, "a");
        assert!(recipient(&snapshot, "reviewer").is_err());
        assert!(recipient(&snapshot, "missing").is_err());
        assert!(identifier("../other").is_err());
        Ok(())
    }
}
