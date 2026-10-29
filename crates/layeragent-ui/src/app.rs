//! Interface state: what is on screen, how keys change it and how agent events fill it.
//! Nothing here touches the terminal or the network, so it is fully testable.
use crate::mascot::Pose;
use serde_json::Value;

pub const GREETING_SECS: f32 = 2.4;
pub const FAREWELL_SECS: f32 = 1.9;
const CELEBRATE_SECS: f32 = 1.5;
const TIP_SECS: f32 = 9.0;

pub const TIPS: [&str; 6] = [
    "escribe /help para ver todo lo que puedes hacer",
    "pulsa 1, 2 o 3 para empezar con una idea lista",
    "Esc cancela lo que el agente esté haciendo en ese momento",
    "pídele que busque algo en la web: tiene su propio navegador",
    "puede repartir el trabajo entre varios agentes a la vez",
    "arrastra un archivo a la terminal para citar su ruta",
];

pub const STARTERS: [(&str, &str, &str); 3] = [
    (
        "Explorar",
        "Resume la estructura y el propósito de este proyecto",
        "Explora este proyecto y explícame su estructura, su propósito y cómo se ejecuta.",
    ),
    (
        "Revisar cambios",
        "Analiza lo que cambió y sugiere mejoras",
        "Revisa los cambios sin confirmar de este repositorio y dime qué mejorarías o qué riesgos ves.",
    ),
    (
        "Buscar problemas",
        "Encuentra errores y riesgos en el código",
        "Busca errores, riesgos de seguridad y malas prácticas en este proyecto y propón correcciones.",
    ),
];

pub const PHRASES: [&str; 6] = [
    "meditando",
    "conectando ideas",
    "tejiendo el código",
    "consultando las nubes",
    "afinando las alas",
    "escuchando al proyecto",
];

#[derive(Clone, Debug, Default)]
pub struct Info {
    pub model: String,
    pub project: String,
    pub path: String,
    pub os: String,
    pub shell: String,
    pub version: String,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Key {
    Char(char),
    Ctrl(char),
    Enter,
    Esc,
    Backspace,
    Delete,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    Tab,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    None,
    Send(String),
    Cancel,
    Approve {
        id: String,
        allow: bool,
        expected_revision: i64,
        arguments_hash: String,
    },
    NewSession,
    Context,
    Compact,
    Effort(Option<String>),
    Team(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum ToolState {
    Running,
    Done { ok: bool, secs: f32 },
}

#[derive(Clone, Debug)]
pub enum Entry {
    User(String),
    Assistant(String),
    Tool {
        id: String,
        name: String,
        agent: Option<String>,
        summary: String,
        state: ToolState,
        started: f32,
        output: Vec<String>,
    },
    Changes {
        files: Vec<(String, String)>,
        partial: bool,
    },
    Agent {
        name: String,
        status: String,
    },
    Note(String),
    Error(String),
}

#[derive(Clone, Debug)]
pub struct Approval {
    pub id: String,
    pub tool: String,
    pub detail: Vec<String>,
    pub agent: Option<String>,
    pub scroll: usize,
    pub viewport: usize,
    pub total_rows: usize,
    pub reviewed_through: usize,
    pub layout_width: usize,
    pub review_available: bool,
    pub review_error: Option<String>,
    /// Revision and argument fingerprint the daemon requires in the decision.
    pub expected_revision: i64,
    pub arguments_hash: String,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Screen {
    Home,
    Chat,
}

pub struct App {
    pub info: Info,
    pub(crate) screen: Screen,
    pub(crate) clock: f32,
    pub(crate) input: Vec<char>,
    pub(crate) cursor: usize,
    sent: Vec<String>,
    history_at: Option<usize>,
    pub(crate) entries: Vec<Entry>,
    pub(crate) streaming: String,
    pub(crate) busy: bool,
    pub(crate) busy_since: f32,
    pub(crate) approval: Option<Approval>,
    /// Lines scrolled up from the bottom; 0 follows new output.
    pub(crate) scroll: usize,
    pub(crate) card: usize,
    pub(crate) help: bool,
    pub execution: String,
    pub team: Option<Value>,
    pub team_visible: bool,
    pub team_resource: String,
    pub(crate) team_notice: Option<String>,
    pub(crate) team_scroll: usize,
    team_event_seq: i64,
    intro_at: Option<f32>,
    celebrate_at: Option<f32>,
    farewell_at: Option<f32>,
    pub(crate) tip_epoch: f32,
}

impl App {
    pub fn new(info: Info) -> Self {
        Self {
            info,
            screen: Screen::Home,
            clock: 0.0,
            input: Vec::new(),
            cursor: 0,
            sent: Vec::new(),
            history_at: None,
            entries: Vec::new(),
            streaming: String::new(),
            busy: false,
            busy_since: 0.0,
            approval: None,
            scroll: 0,
            card: 0,
            help: false,
            execution: "delegated".into(),
            team: None,
            team_visible: false,
            team_resource: "members".into(),
            team_notice: None,
            team_scroll: 0,
            team_event_seq: 0,
            intro_at: None,
            celebrate_at: None,
            farewell_at: None,
            tip_epoch: 0.0,
        }
    }

    pub fn tick(&mut self, t: f32) {
        self.clock = t;
        self.intro_at.get_or_insert(t);
    }

    pub fn is_busy(&self) -> bool {
        self.busy
    }

    pub fn screen(&self) -> Screen {
        self.screen
    }

    pub fn leaving(&self) -> bool {
        self.farewell_at.is_some()
    }

    /// True once the goodbye animation has finished and the program should exit.
    pub fn finished(&self) -> bool {
        self.farewell_at
            .is_some_and(|at| self.clock - at >= FAREWELL_SECS)
    }

    pub fn begin_farewell(&mut self) {
        self.farewell_at.get_or_insert(self.clock);
    }

    pub fn input_text(&self) -> String {
        self.input.iter().collect()
    }

    pub fn tip_index(&self) -> usize {
        ((self.clock - self.tip_epoch) / TIP_SECS) as usize % TIPS.len()
    }

    pub fn phrase_index(&self) -> usize {
        ((self.clock - self.busy_since) / 2.6) as usize % PHRASES.len()
    }

    /// Which pose the mascot should hold right now, and what it says.
    pub fn mascot(&self) -> (Pose, Option<&'static str>) {
        let t = self.clock;
        if let Some(at) = self.farewell_at {
            let p = ((t - at) / FAREWELL_SECS).clamp(0.0, 1.0);
            return (Pose::farewell(t, p), (p < 0.7).then_some("¡Hasta pronto!"));
        }
        if self.busy {
            return (Pose::thinking(t), None);
        }
        if let Some(at) = self.celebrate_at
            && t - at < CELEBRATE_SECS
        {
            let p = (t - at) / CELEBRATE_SECS;
            return (Pose::greeting(t, p), Some("¡Listo!"));
        }
        if let Some(at) = self.intro_at
            && t - at < GREETING_SECS
        {
            let p = (t - at) / GREETING_SECS;
            return (Pose::greeting(t, p), Some("¡Hola! Soy LayerAgent"));
        }
        (Pose::idle(t), None)
    }

    fn reset_input(&mut self) {
        self.input.clear();
        self.cursor = 0;
        self.history_at = None;
    }

    fn submit(&mut self, text: String) -> Action {
        let text = text.trim().to_string();
        self.reset_input();
        if text.is_empty() {
            return Action::None;
        }
        match text.as_str() {
            "/help" | "/ayuda" | "/?" => {
                self.help = !self.help;
                return Action::None;
            }
            "/exit" | "/salir" | "/quit" => {
                self.begin_farewell();
                return Action::None;
            }
            "/clear" | "/limpiar" => {
                self.entries.clear();
                self.streaming.clear();
                self.scroll = 0;
                return Action::None;
            }
            "/new" | "/nuevo" => {
                if self.busy {
                    self.note("Espera a que termine o cancela antes de crear otra sesión.");
                    return Action::None;
                }
                return Action::NewSession;
            }
            _ => {}
        }
        if text == "/context" {
            return Action::Context;
        }
        if text == "/execution" {
            self.note(&format!("Modo de las próximas tareas: {}", self.execution));
            return Action::None;
        }
        if let Some(mode) = text.strip_prefix("/execution ") {
            if matches!(mode, "individual" | "delegated" | "cooperative") {
                self.execution = mode.into();
                self.note(&format!("Próximas tareas: {mode}"));
            } else {
                self.note("Uso: /execution individual|delegated|cooperative");
            }
            return Action::None;
        }
        if text == "/team" || text == "/team toggle" {
            self.team_visible = !self.team_visible;
            self.screen = Screen::Chat;
            return Action::None;
        }
        if let Some(command) = text.strip_prefix("/team ") {
            if matches!(
                command,
                "members"
                    | "messages"
                    | "work"
                    | "findings"
                    | "objections"
                    | "evidence"
                    | "decisions"
            ) {
                self.team_resource = command.into();
                self.team_visible = true;
                self.team_scroll = 0;
                self.screen = Screen::Chat;
                return Action::None;
            }
            if command.starts_with("start ") && self.busy {
                self.note(
                    "Ya hay una tarea activa; usa /team attach para inspeccionar otro equipo.",
                );
                return Action::None;
            }
            return Action::Team(command.into());
        }
        if text == "/effort" {
            return Action::Effort(None);
        }
        if let Some(level) = text.strip_prefix("/effort ") {
            if matches!(level.trim(), "auto" | "low" | "medium" | "high" | "xhigh") {
                return Action::Effort(Some(level.trim().into()));
            }
            self.note("Uso: /effort auto|low|medium|high|xhigh");
            return Action::None;
        }
        if text == "/compact" {
            if self.busy {
                self.note("Espera a que termine la tarea para compactar.");
                return Action::None;
            }
            return Action::Compact;
        }
        if text.starts_with('/') {
            self.note("Comando desconocido. Usa /help para consultar los comandos disponibles.");
            return Action::None;
        }
        if self.busy {
            self.note(
                "El agente sigue trabajando: espera a que termine o pulsa Esc para cancelar.",
            );
            return Action::None;
        }
        self.help = false;
        self.team = None;
        self.team_notice = None;
        self.team_visible = self.execution == "cooperative";
        self.team_event_seq = 0;
        self.sent.push(text.clone());
        self.screen = Screen::Chat;
        self.entries.push(Entry::User(text.clone()));
        self.streaming.clear();
        self.busy = true;
        self.busy_since = self.clock;
        self.scroll = 0;
        Action::Send(text)
    }

    /// Clears the conversation after a new session has been opened.
    pub fn reset_conversation(&mut self) {
        self.entries.clear();
        self.streaming.clear();
        self.screen = Screen::Home;
        self.scroll = 0;
        self.busy = false;
        self.approval = None;
        self.team = None;
        self.team_visible = false;
        self.team_event_seq = 0;
        self.intro_at = Some(self.clock);
    }

    pub fn load_history(&mut self, messages: &[Value]) {
        self.entries.clear();
        for message in messages {
            let text = message["content"].as_str().unwrap_or_default();
            match message["role"].as_str() {
                Some("user") => self.entries.push(Entry::User(text.into())),
                Some("assistant") if !text.is_empty() => {
                    self.entries.push(Entry::Assistant(text.into()))
                }
                _ => {}
            }
        }
        if !self.entries.is_empty() {
            self.screen = Screen::Chat;
        }
    }

    pub fn note(&mut self, text: &str) {
        self.entries.push(Entry::Note(text.to_string()));
    }

    pub fn begin_team(&mut self, objective: &str) {
        self.screen = Screen::Chat;
        self.busy = true;
        self.busy_since = self.clock;
        self.team = None;
        self.team_notice = None;
        self.team_visible = true;
        self.team_event_seq = 0;
        self.entries.push(Entry::User(objective.into()));
    }

    pub fn team_note(&mut self, text: &str) {
        self.team_notice = Some(text.into());
        self.note(text);
    }

    pub fn on_key(&mut self, key: Key) -> Action {
        if self.leaving() {
            return Action::None;
        }
        if let Some(approval) = &mut self.approval {
            let id = approval.id.clone();
            let expected_revision = approval.expected_revision;
            let arguments_hash = approval.arguments_hash.clone();
            return match key {
                Key::Char('y' | 'Y' | 's' | 'S') | Key::Enter
                    if approval.review_available
                        && approval.review_error.is_none()
                        && approval.reviewed_through >= approval.total_rows =>
                {
                    self.approval = None;
                    Action::Approve {
                        id,
                        allow: true,
                        expected_revision,
                        arguments_hash,
                    }
                }
                Key::Down => {
                    approval.scroll = approval
                        .scroll
                        .saturating_add(1)
                        .min(approval.total_rows.saturating_sub(approval.viewport));
                    Action::None
                }
                Key::Up => {
                    approval.scroll = approval.scroll.saturating_sub(1);
                    Action::None
                }
                Key::PageDown => {
                    approval.scroll = approval
                        .scroll
                        .saturating_add(approval.viewport.max(1))
                        .min(approval.total_rows.saturating_sub(approval.viewport));
                    Action::None
                }
                Key::PageUp => {
                    approval.scroll = approval.scroll.saturating_sub(approval.viewport.max(1));
                    Action::None
                }
                Key::Home => {
                    approval.scroll = 0;
                    Action::None
                }
                Key::End => {
                    approval.scroll = approval.total_rows.saturating_sub(approval.viewport);
                    Action::None
                }
                Key::Char('n' | 'N') | Key::Esc => {
                    self.approval = None;
                    Action::Approve {
                        id,
                        allow: false,
                        expected_revision,
                        arguments_hash,
                    }
                }
                Key::Ctrl('c') => {
                    self.approval = None;
                    Action::Cancel
                }
                _ => Action::None,
            };
        }
        match key {
            Key::Ctrl('c') => {
                if self.busy {
                    self.note("Cancelando…");
                    return Action::Cancel;
                }
                self.begin_farewell();
                Action::None
            }
            Key::Esc => {
                if self.help {
                    self.help = false;
                    Action::None
                } else if self.busy {
                    self.note("Cancelando…");
                    Action::Cancel
                } else {
                    self.reset_input();
                    Action::None
                }
            }
            Key::Enter => {
                if self.input.is_empty() && self.screen == Screen::Home {
                    return self.submit(STARTERS[self.card].2.to_string());
                }
                let text = self.input_text();
                self.submit(text)
            }
            Key::Char(d @ '1'..='3') if self.input.is_empty() && self.screen == Screen::Home => {
                self.card = d as usize - '1' as usize;
                self.submit(STARTERS[self.card].2.to_string())
            }
            Key::Tab if self.input.is_empty() => {
                self.card = (self.card + 1) % STARTERS.len();
                Action::None
            }
            Key::Left if self.input.is_empty() && self.screen == Screen::Home => {
                self.card = (self.card + STARTERS.len() - 1) % STARTERS.len();
                Action::None
            }
            Key::Right if self.input.is_empty() && self.screen == Screen::Home => {
                self.card = (self.card + 1) % STARTERS.len();
                Action::None
            }
            Key::Left => {
                self.cursor = self.cursor.saturating_sub(1);
                Action::None
            }
            Key::Right => {
                self.cursor = (self.cursor + 1).min(self.input.len());
                Action::None
            }
            Key::Home | Key::Ctrl('a') => {
                self.cursor = 0;
                Action::None
            }
            Key::End | Key::Ctrl('e') => {
                self.cursor = self.input.len();
                Action::None
            }
            Key::Ctrl('u') => {
                self.input.drain(..self.cursor);
                self.cursor = 0;
                Action::None
            }
            Key::Ctrl('w') => {
                let mut start = self.cursor;
                while start > 0 && self.input[start - 1] == ' ' {
                    start -= 1;
                }
                while start > 0 && self.input[start - 1] != ' ' {
                    start -= 1;
                }
                self.input.drain(start..self.cursor);
                self.cursor = start;
                Action::None
            }
            Key::Backspace => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                    self.input.remove(self.cursor);
                }
                Action::None
            }
            Key::Delete => {
                if self.cursor < self.input.len() {
                    self.input.remove(self.cursor);
                }
                Action::None
            }
            Key::Up => {
                if self.sent.is_empty() {
                    return Action::None;
                }
                let at = self
                    .history_at
                    .map_or(self.sent.len() - 1, |i| i.saturating_sub(1));
                self.history_at = Some(at);
                self.input = self.sent[at].chars().collect();
                self.cursor = self.input.len();
                Action::None
            }
            Key::Down => {
                if let Some(i) = self.history_at {
                    if i + 1 < self.sent.len() {
                        self.history_at = Some(i + 1);
                        self.input = self.sent[i + 1].chars().collect();
                    } else {
                        self.history_at = None;
                        self.input.clear();
                    }
                    self.cursor = self.input.len();
                }
                Action::None
            }
            Key::PageUp => {
                if self.team_visible {
                    self.team_scroll = self.team_scroll.saturating_sub(8);
                    return Action::None;
                }
                self.scroll = self.scroll.saturating_add(8);
                Action::None
            }
            Key::PageDown => {
                if self.team_visible {
                    self.team_scroll = self.team_scroll.saturating_add(8);
                    return Action::None;
                }
                self.scroll = self.scroll.saturating_sub(8);
                Action::None
            }
            Key::Char(c) if !c.is_control() => {
                self.input.insert(self.cursor, c);
                self.cursor += 1;
                self.history_at = None;
                Action::None
            }
            _ => Action::None,
        }
    }

    /// Pasted text arrives as one chunk; newlines become spaces so it stays a single line.
    pub fn on_paste(&mut self, text: &str) {
        for c in text
            .chars()
            .map(|c| {
                if c == '\n' || c == '\r' || c == '\t' {
                    ' '
                } else {
                    c
                }
            })
            .filter(|c| !c.is_control())
        {
            self.input.insert(self.cursor, c);
            self.cursor += 1;
        }
    }

    fn flush_stream(&mut self) {
        if !self.streaming.trim().is_empty() {
            let text = std::mem::take(&mut self.streaming);
            self.entries
                .push(Entry::Assistant(text.trim_end().to_string()));
        } else {
            self.streaming.clear();
        }
    }

    fn finish(&mut self) {
        self.busy = false;
        self.approval = None;
    }

    /// Applies one event from the runtime's stream.
    pub fn on_event(&mut self, event: &Value) {
        if event["kind"] == "team_snapshot" {
            let value = event["data"].clone();
            let changed_task = self
                .team
                .as_ref()
                .is_none_or(|previous| previous["team"]["task_id"] != value["team"]["task_id"]);
            if changed_task {
                self.team_event_seq = value["team"]["last_event_seq"].as_i64().unwrap_or(0);
                self.approval = None;
            }
            self.busy = matches!(value["team"]["state"].as_str(), Some("active" | "closing"))
                || value["team"]["pause_pending"] == true;
            self.team = Some(value.clone());
            if changed_task {
                self.team_visible = true;
            }
            self.screen = Screen::Chat;
            if self.approval.is_none()
                && let Some(approval) = value["team"]["pending_approvals"]
                    .as_array()
                    .and_then(|v| v.first())
            {
                self.on_event(&serde_json::json!({"kind":"approval_required","data":approval}));
            }
            return;
        }
        if event["kind"] == "task_outcome" {
            self.finish();
            self.note(&format!(
                "Equipo {}. {}",
                event["data"]["state"].as_str().unwrap_or("detenido"),
                event["data"]["reason"].as_str().unwrap_or("")
            ));
            return;
        }
        if event["kind"]
            .as_str()
            .is_some_and(|kind| kind.starts_with("team_"))
        {
            if let Some(seq) = event["seq"].as_i64() {
                if seq <= self.team_event_seq {
                    return;
                }
                self.team_event_seq = seq;
            }
            if event["kind"] == "team_member_event" {
                let member = event["data"]["member_id"].as_str().unwrap_or("agente");
                let activation = event["data"]["activation_id"].as_str().unwrap_or("");
                let mut data = event["data"]["data"].clone();
                if let Some(object) = data.as_object_mut() {
                    object.insert("agent_name".into(), Value::String(member.into()));
                    // Provider call ids may repeat in different member sessions.
                    let key = if event["data"]["kind"] == "tool_started" {
                        "id"
                    } else {
                        "call_id"
                    };
                    if let Some(id) = object.get(key).and_then(Value::as_str) {
                        object.insert(
                            key.into(),
                            Value::String(format!("{activation}:{member}:{id}")),
                        );
                    }
                }
                // Per-member text remains in its own session. The shared timeline
                // shows attributable tool events and durable team decisions.
                if event["data"]["kind"] != "text_delta"
                    && event["data"]["kind"] != "turn_completed"
                {
                    self.on_event(&serde_json::json!({"kind":event["data"]["kind"],"data":data}));
                }
                return;
            }
            let kind = event["kind"].as_str().unwrap_or("team");
            self.note(&format!("{kind}: {}", event["data"]));
            if matches!(
                kind,
                "team_completed" | "team_cancelled" | "team_failed" | "team_interrupted"
            ) {
                self.finish();
            }
            return;
        }
        let data = &event["data"];
        match event["kind"].as_str().unwrap_or("") {
            "text_delta" => self.streaming.push_str(data["text"].as_str().unwrap_or("")),
            "attempt_discarded" => {
                self.streaming.clear();
                self.note("El proveedor falló; cambiando a una alternativa autorizada.");
            }
            "tool_started" => {
                self.flush_stream();
                let name = data["name"].as_str().unwrap_or("herramienta").to_string();
                self.entries.push(Entry::Tool {
                    id: data["id"].as_str().unwrap_or("").to_string(),
                    summary: summarize(&name, &data["arguments"]),
                    name,
                    agent: data["agent_name"].as_str().map(str::to_owned),
                    state: ToolState::Running,
                    started: self.clock,
                    output: Vec::new(),
                });
            }
            "tool_output" => {
                let id = data["call_id"].as_str().unwrap_or("");
                if let Some(Entry::Tool { output, .. }) = self
                    .entries
                    .iter_mut()
                    .rev()
                    .find(|e| matches!(e, Entry::Tool { id: i, .. } if i == id))
                {
                    for line in data["text"].as_str().unwrap_or("").lines() {
                        output.push(line.to_string());
                    }
                    let excess = output.len().saturating_sub(40);
                    output.drain(..excess);
                }
            }
            "tool_completed" => {
                let id = data["call_id"].as_str().unwrap_or("");
                let result = &data["result"];
                let ok = result.get("error").is_none() && result["code"].as_i64().unwrap_or(0) == 0;
                let now = self.clock;
                if let Some(Entry::Tool { state, started, .. }) = self
                    .entries
                    .iter_mut()
                    .rev()
                    .find(|e| matches!(e, Entry::Tool { id: i, .. } if i == id))
                {
                    *state = ToolState::Done {
                        ok,
                        secs: now - *started,
                    };
                }
            }
            "approval_required" => {
                let call = &data["call"];
                let tool = call["name"].as_str().unwrap_or("acción").to_string();
                let review = approval_review(data);
                self.approval = Some(Approval {
                    id: data["approval_id"].as_str().unwrap_or("").to_string(),
                    detail: review
                        .as_ref()
                        .map(|text| text.lines().map(str::to_string).collect())
                        .unwrap_or_default(),
                    review_error: review.err(),
                    scroll: 0,
                    viewport: 0,
                    total_rows: 0,
                    reviewed_through: 0,
                    layout_width: 0,
                    review_available: false,
                    expected_revision: data["expected_revision"].as_i64().unwrap_or(0),
                    arguments_hash: data["arguments_hash"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string(),
                    tool,
                    agent: data["agent_name"].as_str().map(str::to_string),
                });
            }
            "approval_resolved" => {
                if self
                    .approval
                    .as_ref()
                    .is_some_and(|approval| data["approval_id"] == approval.id)
                {
                    self.approval = None;
                }
            }
            "workspace_diff" => {
                let files = changed_files(&data["changes"]);
                if !files.is_empty() {
                    self.entries.push(Entry::Changes {
                        files,
                        partial: data["partial"] == true,
                    });
                }
            }
            "subagent_started" => self.entries.push(Entry::Agent {
                name: data["name"].as_str().unwrap_or("agente").to_string(),
                status: "empezó a trabajar".into(),
            }),
            "subagent_completed" => self.entries.push(Entry::Agent {
                name: data["name"].as_str().unwrap_or("agente").to_string(),
                status: match data["status"].as_str() {
                    Some("completed") => "terminó".to_string(),
                    Some(other) => other.to_string(),
                    None => "terminó".to_string(),
                },
            }),
            _ => {}
        }
        if let Some(text) = event["text"].as_str() {
            self.streaming.clear();
            self.entries
                .push(Entry::Assistant(text.trim_end().to_string()));
            self.finish();
            self.celebrate_at = Some(self.clock);
        }
        if let Some(error) = event["error"].as_str() {
            self.flush_stream();
            self.entries.push(Entry::Error(error.to_string()));
            self.finish();
        }
    }
}

fn clip(text: &str, max: usize) -> String {
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        flat
    } else {
        format!(
            "{}…",
            flat.chars().take(max.saturating_sub(1)).collect::<String>()
        )
    }
}

/// A one-line description of a tool call.
pub fn summarize(name: &str, args: &Value) -> String {
    let pick = |keys: &[&str]| {
        keys.iter()
            .find_map(|k| args[*k].as_str())
            .map(|s| clip(s, 70))
    };
    match name {
        "shell" => pick(&["command"]),
        "read_file" | "write_file" | "edit_file" | "list_files" => pick(&["path"]),
        "search_files" | "browser_search" | "memory_search" => pick(&["query"]),
        "browser_open" => pick(&["url"]),
        "spawn_agent" => pick(&["task"]),
        "git_commit" => pick(&["message"]),
        _ => None,
    }
    .unwrap_or_else(|| {
        let s = args.to_string();
        if s == "{}" || s == "null" {
            String::new()
        } else {
            clip(&s, 70)
        }
    })
}

/// Full review text shared by terminal clients. JSON escaping neutralizes terminal
/// control characters. Oversized rendered records are refused rather than truncated.
pub fn approval_review(data: &Value) -> Result<String, String> {
    struct Bounded(Vec<u8>);
    impl std::io::Write for Bounded {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self.0.len().saturating_add(bytes.len()) > 1_048_576 {
                return Err(std::io::Error::other("approval review exceeds 1 MiB"));
            }
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut out = Bounded(Vec::new());
    serde_json::to_writer_pretty(&mut out, data).map_err(|_| {
        "Revisión demasiado grande: aprobación bloqueada; deniega y solicita una acción menor."
            .to_string()
    })?;
    let text = String::from_utf8(out.0)
        .map_err(|_| "Revisión inválida: aprobación bloqueada.".to_string())?;
    let mut safe = Bounded(Vec::new());
    use std::io::Write;
    for c in text.chars() {
        let escaped;
        let mut buffer = [0; 4];
        let part = if c != '\n'
            && (c.is_control() || unicode_width::UnicodeWidthChar::width(c) == Some(0))
        {
            escaped = c.escape_unicode().to_string();
            escaped.as_str()
        } else {
            c.encode_utf8(&mut buffer)
        };
        safe.write_all(part.as_bytes())
            .map_err(|_| "Revisión demasiado grande: aprobación bloqueada.".to_string())?;
    }
    String::from_utf8(safe.0).map_err(|_| "Revisión inválida: aprobación bloqueada.".to_string())
}

fn changed_files(changes: &Value) -> Vec<(String, String)> {
    changes
        .as_array()
        .map(|files| {
            files
                .iter()
                .map(|f| {
                    (
                        f["kind"].as_str().unwrap_or("modified").to_string(),
                        f["path"].as_str().unwrap_or("").to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn app() -> App {
        let mut app = App::new(Info::default());
        app.tick(0.0);
        app
    }
    fn type_str(app: &mut App, text: &str) {
        for c in text.chars() {
            app.on_key(Key::Char(c));
        }
    }

    #[test]
    fn terminal_commands_never_become_model_prompts() {
        let mut app = App::new(Info::default());
        assert_eq!(
            app.submit("/effort high".into()),
            Action::Effort(Some("high".into()))
        );
        assert_eq!(app.submit("/context".into()), Action::Context);
        assert_eq!(app.submit("/compact".into()), Action::Compact);
        assert_eq!(app.submit("/effort invalid".into()), Action::None);
        assert_eq!(app.submit("/bridge on".into()), Action::None);
        app.busy = true;
        assert_eq!(app.submit("/compact".into()), Action::None);
        assert_eq!(app.submit("/new".into()), Action::None);
        assert_eq!(
            app.submit("/effort low".into()),
            Action::Effort(Some("low".into()))
        );
    }

    #[test]
    fn typing_editing_and_history() {
        let mut a = app();
        type_str(&mut a, "hola mundo");
        a.on_key(Key::Left);
        a.on_key(Key::Backspace);
        assert_eq!(a.input_text(), "hola muno");
        a.on_key(Key::Ctrl('w'));
        assert_eq!(a.input_text(), "hola o");
        a.on_key(Key::Ctrl('u'));
        assert_eq!(a.input_text(), "o");
        a.on_key(Key::Delete);
        type_str(&mut a, "una pregunta");
        assert_eq!(a.on_key(Key::Enter), Action::Send("una pregunta".into()));
        assert!(a.is_busy() && a.screen() == Screen::Chat);
        a.on_event(&json!({"text":"respuesta"}));
        a.on_key(Key::Up);
        assert_eq!(a.input_text(), "una pregunta");
    }

    #[test]
    fn starter_cards_launch_with_a_number_or_enter() {
        let mut a = app();
        assert!(matches!(a.on_key(Key::Char('2')), Action::Send(p) if p.contains("cambios")));
        let mut b = app();
        b.on_key(Key::Tab);
        assert!(matches!(b.on_key(Key::Enter), Action::Send(p) if p.contains("cambios")));
        // Digits are ordinary text once something is typed.
        let mut c = app();
        type_str(&mut c, "a2");
        assert_eq!(c.input_text(), "a2");
    }

    #[test]
    fn slash_commands_do_not_reach_the_agent() {
        let mut a = app();
        type_str(&mut a, "/help");
        assert_eq!(a.on_key(Key::Enter), Action::None);
        assert!(a.help);
        type_str(&mut a, "/new");
        assert_eq!(a.on_key(Key::Enter), Action::NewSession);
        type_str(&mut a, "/exit");
        a.on_key(Key::Enter);
        assert!(a.leaving());
        a.tick(0.0 + FAREWELL_SECS + 0.1);
        assert!(a.finished());
    }

    #[test]
    fn approval_review_escapes_terminal_and_invisible_controls_without_losing_suffix() {
        let rendered = approval_review(&json!({"call":{"arguments":{"command":"safe\u{1b}[2J\u{202e}DANGER"}},"destination":"https://service.example/mcp","action_context":{"url":"https://page.example"},"arguments_hash":"hash","expires_at":123})).expect("bounded review");
        assert!(!rendered.contains('\u{1b}'));
        assert!(!rendered.contains('\u{202e}'));
        for expected in [
            "DANGER",
            "service.example/mcp",
            "page.example",
            "arguments_hash",
            "expires_at",
        ] {
            assert!(rendered.contains(expected));
        }
    }
    #[test]
    fn events_build_a_conversation_with_tools_and_approvals() {
        let mut a = app();
        type_str(&mut a, "haz algo");
        a.on_key(Key::Enter);
        a.on_event(&json!({"kind":"text_delta","data":{"text":"Voy a mirar. "}}));
        a.on_event(&json!({"kind":"tool_started","data":{"id":"c1","name":"shell","arguments":{"command":"git status"}}}));
        a.on_event(
            &json!({"kind":"tool_output","data":{"call_id":"c1","text":"On branch main\nclean\n"}}),
        );
        a.tick(2.0);
        a.on_event(&json!({"kind":"tool_completed","data":{"call_id":"c1","result":{"code":0}}}));
        a.on_event(&json!({"kind":"approval_required","data":{"approval_id":"ap1","call":{"name":"shell","arguments":{"command":"rm -rf build"}},"agent_name":"worker"}}));
        assert!(a.approval.is_some());
        assert_eq!(
            a.on_key(Key::Char('n')),
            Action::Approve {
                id: "ap1".into(),
                allow: false,
                expected_revision: 0,
                arguments_hash: String::new(),
            }
        );
        a.on_event(
            &json!({"kind":"workspace_diff","data":{"changes":[{"kind":"added","path":"a.txt"}]}}),
        );
        a.on_event(&json!({"text":"Hecho."}));
        assert!(!a.is_busy());
        let kinds: Vec<_> = a
            .entries
            .iter()
            .map(|e| match e {
                Entry::User(_) => "user",
                Entry::Assistant(_) => "assistant",
                Entry::Tool { .. } => "tool",
                Entry::Changes { .. } => "changes",
                _ => "other",
            })
            .collect();
        assert_eq!(kinds, ["user", "assistant", "tool", "changes", "assistant"]);
        match &a.entries[2] {
            Entry::Tool {
                summary,
                state,
                output,
                ..
            } => {
                assert_eq!(summary, "git status");
                assert!(matches!(state, ToolState::Done { ok: true, secs } if *secs > 1.9));
                assert_eq!(output.len(), 2);
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn mascot_moods_follow_what_the_agent_is_doing() {
        let mut a = app();
        let (greeting, say) = a.mascot();
        assert!(greeting.wave >= 0.0 && say.is_some(), "greets on entry");
        a.tick(GREETING_SECS + 1.0);
        let (idle, say) = a.mascot();
        assert!(idle.think == 0.0 && say.is_none());
        type_str(&mut a, "x");
        a.on_key(Key::Enter);
        assert!(a.mascot().0.think > 0.9, "concentrates while working");
        a.on_event(&json!({"text":"ok"}));
        assert_eq!(a.mascot().1, Some("¡Listo!"));
        a.on_key(Key::Ctrl('c'));
        assert_eq!(a.mascot().1, Some("¡Hasta pronto!"));
    }

    #[test]
    fn ctrl_c_cancels_work_before_it_quits_and_enter_is_ignored_while_busy() {
        let mut a = app();
        type_str(&mut a, "x");
        a.on_key(Key::Enter);
        type_str(&mut a, "otra");
        assert_eq!(a.on_key(Key::Enter), Action::None);
        assert_eq!(a.on_key(Key::Ctrl('c')), Action::Cancel);
        assert!(!a.leaving());
        a.on_event(&json!({"error":"cancelado"}));
        a.on_key(Key::Ctrl('c'));
        assert!(a.leaving());
    }

    #[test]
    fn summaries_are_short_and_paste_stays_on_one_line() {
        assert_eq!(
            summarize("browser_open", &json!({"url":"https://x.dev"})),
            "https://x.dev"
        );
        assert_eq!(summarize("list_agents", &json!({})), "");
        let mut a = app();
        a.on_paste("línea uno\nlínea dos\t fin");
        assert_eq!(a.input_text(), "línea uno línea dos  fin");
    }
    #[test]
    fn team_controls_work_while_busy_and_never_become_prompts() {
        let mut a = app();
        a.begin_team("investigate");
        type_str(&mut a, "/team message reviewer check this claim");
        assert_eq!(
            a.on_key(Key::Enter),
            Action::Team("message reviewer check this claim".into())
        );
        type_str(&mut a, "/team pause");
        assert_eq!(a.on_key(Key::Enter), Action::Team("pause".into()));
        type_str(&mut a, "/execution individual");
        assert_eq!(a.on_key(Key::Enter), Action::None);
        assert_eq!(a.execution, "individual");
        type_str(&mut a, "/team start audit -- another task");
        assert_eq!(a.on_key(Key::Enter), Action::None);
    }

    #[test]
    fn team_snapshots_preserve_toggle_and_events_are_deduplicated() {
        let mut a = app();
        let snapshot = json!({"kind":"team_snapshot","data":{"team":{"task_id":"one","state":"active","last_event_seq":12,"members":[]}}});
        a.on_event(&snapshot);
        assert!(a.busy && a.team_visible);
        type_str(&mut a, "/team toggle");
        a.on_key(Key::Enter);
        a.on_event(&snapshot);
        assert!(!a.team_visible);
        let event =
            json!({"seq":13,"kind":"team_message","data":{"sender":"reviewer","body":"challenge"}});
        a.on_event(&event);
        let count = a.entries.len();
        a.on_event(&event);
        assert_eq!(a.entries.len(), count);
        a.on_event(&json!({"kind":"task_outcome","data":{"state":"completed"}}));
        assert!(!a.busy);
        assert!(a.team.is_some(), "completed team remains inspectable");
    }

    #[test]
    fn attach_restores_an_outstanding_approval_with_its_revision() {
        let mut a = app();
        a.on_event(&json!({"kind":"team_snapshot","data":{"team":{"task_id":"one","state":"active","last_event_seq":8,"pending_approvals":[{"approval_id":"approval-one","member_id":"verifier","expected_revision":7,"arguments_hash":"abc","call":{"name":"shell","arguments":{"command":"printf check"}}}]}}}));
        let approval = a.approval.as_ref().expect("snapshot approval");
        assert_eq!(approval.expected_revision, 7);
        assert_eq!(approval.arguments_hash, "abc");
        assert!(matches!(
            a.on_key(Key::Char('n')),
            Action::Approve {
                allow: false,
                expected_revision: 7,
                ..
            }
        ));
    }

    #[test]
    fn delegated_prompt_after_team_completion_clears_cooperative_selection() {
        let mut a = app();
        a.on_event(&json!({"kind":"team_snapshot","data":{"team":{"task_id":"old-team-task","state":"terminated","last_event_seq":8,"members":[]}}}));
        a.team_note("Old team notice");
        type_str(&mut a, "/execution delegated");
        assert_eq!(a.on_key(Key::Enter), Action::None);
        type_str(&mut a, "new independent task");
        assert_eq!(
            a.on_key(Key::Enter),
            Action::Send("new independent task".into())
        );
        assert!(a.team.is_none());
        assert!(!a.team_visible);
        assert!(a.team_notice.is_none());
        assert!(a.is_busy());
    }
}
