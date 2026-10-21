//! Drawing: composes the mascot, wordmark, panels and conversation for the current size.
use crate::app::{App, Entry, PHRASES, STARTERS, Screen, TIPS, ToolState};
use crate::logo;
use crate::mascot;
use crate::raster::Rgb;
use crate::theme::{
    self, ACCENT, ACCENT_LIGHT, AMBER, BORDER, DIM, FAINT, GREEN, RED, TEXT, WHITE, bold, c, dim,
    fg,
};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Clear, Padding, Paragraph},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Layout3 {
    Wide,
    Medium,
    Narrow,
    Tiny,
}

pub fn classify(area: Rect) -> Layout3 {
    match (area.width, area.height) {
        (w, h) if w >= 150 && h >= 36 => Layout3::Wide,
        (w, h) if w >= 100 && h >= 30 => Layout3::Medium,
        (w, h) if w >= 64 && h >= 22 => Layout3::Narrow,
        _ => Layout3::Tiny,
    }
}

fn rect(x: u16, y: u16, w: u16, h: u16, within: Rect) -> Rect {
    let x = x.min(within.right());
    let y = y.min(within.bottom());
    Rect::new(x, y, w.min(within.right() - x), h.min(within.bottom() - y))
}

fn width_of(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

fn truncate(s: &str, max: usize) -> String {
    if width_of(s) <= max {
        return s.to_string();
    }
    let mut out = String::new();
    let mut w = 0;
    for ch in s.chars() {
        let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
        if w + cw + 1 > max {
            break;
        }
        out.push(ch);
        w += cw;
    }
    out.push('…');
    out
}

/// Greedy word wrap that also splits words longer than the line.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(4);
    let mut lines = Vec::new();
    for paragraph in text.split('\n') {
        let mut line = String::new();
        let mut line_w = 0;
        for word in paragraph.split(' ') {
            let ww = width_of(word);
            let gap = usize::from(line_w > 0);
            if line_w + gap + ww <= width {
                if gap == 1 {
                    line.push(' ');
                }
                line.push_str(word);
                line_w += gap + ww;
                continue;
            }
            if line_w > 0 {
                lines.push(std::mem::take(&mut line));
                line_w = 0;
            }
            if ww <= width {
                line.push_str(word);
                line_w = ww;
            } else {
                for ch in word.chars() {
                    let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
                    if line_w + cw > width {
                        lines.push(std::mem::take(&mut line));
                        line_w = 0;
                    }
                    line.push(ch);
                    line_w += cw;
                }
            }
        }
        lines.push(line);
    }
    lines
}

/// `code` and **bold** inside a line of prose.
fn inline(text: &str, base: Style) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        let tick = rest.find('`');
        let star = rest.find("**");
        let next = match (tick, star) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        let Some(at) = next else {
            spans.push(Span::styled(rest.to_string(), base));
            break;
        };
        if at > 0 {
            spans.push(Span::styled(rest[..at].to_string(), base));
        }
        let (marker, style) = if rest[at..].starts_with("**") {
            ("**", base.add_modifier(Modifier::BOLD).fg(c(WHITE)))
        } else {
            ("`", fg(ACCENT_LIGHT))
        };
        let after = &rest[at + marker.len()..];
        match after.find(marker) {
            Some(end) => {
                spans.push(Span::styled(after[..end].to_string(), style));
                rest = &after[end + marker.len()..];
            }
            None => {
                spans.push(Span::styled(rest[at..].to_string(), base));
                break;
            }
        }
    }
    spans
}

fn shimmer(text: &str, t: f32, base: Rgb, hi: Rgb) -> Vec<Span<'static>> {
    let n = text.chars().count() as f32;
    let sweep = (t * 11.0) % (n + 10.0) - 5.0;
    text.chars()
        .enumerate()
        .map(|(i, ch)| {
            let k = (1.0 - ((i as f32 - sweep).abs() / 4.5)).clamp(0.0, 1.0);
            Span::styled(
                ch.to_string(),
                Style::default()
                    .fg(c(base.lerp(hi, k)))
                    .add_modifier(Modifier::BOLD),
            )
        })
        .collect()
}

fn pulse(t: f32, a: Rgb, b: Rgb) -> Rgb {
    a.lerp(b, 0.5 + 0.5 * (t * 4.0).sin())
}

fn panel(title: &str) -> Block<'static> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(fg(BORDER))
        .padding(Padding::horizontal(1))
        .title(Span::styled(format!(" {title} "), bold(ACCENT_LIGHT)))
}

fn short_path(path: &str, max: usize) -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    let shown = if !home.is_empty() && path.starts_with(&home) {
        format!("~{}", &path[home.len()..])
    } else {
        path.to_string()
    };
    if width_of(&shown) <= max {
        return shown;
    }
    let tail: String = shown
        .chars()
        .rev()
        .take(max.saturating_sub(2))
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("…{tail}")
}

fn draw_mascot(app: &App, f: &mut Frame, area: Rect, with_bubble: bool) {
    if area.width < 8 || area.height < 4 {
        return;
    }
    let (pose, say) = app.mascot();
    let canvas = mascot::render(area.width as usize, area.height as usize * 2, &pose);
    let lines = canvas.to_lines(theme::BACKDROP, theme::truecolor());
    f.render_widget(Paragraph::new(lines), area);
    if let (true, Some(text)) = (with_bubble, say) {
        let w = (width_of(text) + 4) as u16;
        if area.width > w + 2 && area.height > 5 {
            let r = rect(
                area.x + area.width.saturating_sub(w) / 2,
                area.y,
                w,
                3,
                area,
            );
            f.render_widget(Clear, r);
            f.render_widget(
                Paragraph::new(Line::from(Span::styled(text, bold(WHITE))))
                    .centered()
                    .block(
                        Block::bordered()
                            .border_type(BorderType::Rounded)
                            .border_style(fg(ACCENT)),
                    ),
                r,
            );
        }
    }
}

fn features() -> [(&'static str, &'static str, &'static str); 4] {
    [
        ("◈", "Desarrollar", "Genera, edita y refactoriza código"),
        ("⌕", "Analizar", "Revisa código, busca problemas y optimiza"),
        ("☁", "Navegar", "Busca y lee la web con su propio navegador"),
        ("⛭", "Delegar", "Reparte tareas entre agentes en paralelo"),
    ]
}

fn center_column(app: &App, f: &mut Frame, area: Rect, compact: bool) {
    let mut lines: Vec<Line> = Vec::new();
    if area.width < logo::width() as u16 {
        lines.push(Line::from(shimmer(
            "L A Y E R A G E N T",
            app.clock,
            ACCENT_LIGHT,
            WHITE,
        )));
    } else {
        lines.extend(logo::render(app.clock));
    }
    let version = format!("v{}", app.info.version);
    let pad = if compact {
        0
    } else {
        (logo::width()).saturating_sub(version.len() + 1)
    };
    lines.push(Line::from(vec![
        Span::raw(" ".repeat(pad)),
        Span::styled(version, dim()),
    ]));
    lines.push(Line::from(Span::styled(
        "Tu agente de desarrollo con IA",
        bold(ACCENT_LIGHT),
    )));
    lines.push(Line::raw(""));
    for (icon, name, desc) in features() {
        lines.push(Line::from(vec![
            Span::styled(format!("{icon}  "), bold(ACCENT)),
            Span::styled(format!("{name:<14}"), bold(ACCENT_LIGHT)),
            Span::styled(
                truncate(desc, area.width.saturating_sub(20) as usize),
                fg(TEXT),
            ),
        ]));
        if !compact {
            lines.push(Line::raw(""));
        }
    }
    let height = lines.len() as u16;
    let top = area.y + area.height.saturating_sub(height) / 2;
    f.render_widget(
        Paragraph::new(lines),
        rect(area.x, top, area.width, height.min(area.height), area),
    );
}

fn system_panel(app: &App, f: &mut Frame, area: Rect) {
    let row = |k: &str, v: String| {
        Line::from(vec![
            Span::styled(format!("{k:<10}"), dim()),
            Span::styled(v, fg(TEXT)),
        ])
    };
    let inner = area.width.saturating_sub(14) as usize;
    let status = if app.is_busy() {
        Line::from(vec![
            Span::styled("Estado    ".to_string(), dim()),
            Span::styled(
                format!("{} ", SPINNER[(app.clock * 12.0) as usize % 10]),
                bold(AMBER),
            ),
            Span::styled("Trabajando", fg(AMBER)),
        ])
    } else {
        let glow = pulse(app.clock * 0.5, GREEN, Rgb(120, 230, 150));
        Line::from(vec![
            Span::styled("Estado    ".to_string(), dim()),
            Span::styled("● ", fg(glow)),
            Span::styled("Listo para trabajar", fg(GREEN)),
        ])
    };
    let lines = vec![
        row("Modelo", truncate(&app.info.model, inner)),
        row("Proyecto", truncate(&app.info.project, inner)),
        row("Ruta", short_path(&app.info.path, inner)),
        row("SO", truncate(&app.info.os, inner)),
        row("Shell", truncate(&app.info.shell, inner)),
        status,
    ];
    f.render_widget(Paragraph::new(lines).block(panel("Sistema")), area);
}

fn quick_panel(f: &mut Frame, area: Rect) {
    let row = |k: &str, v: &str| {
        Line::from(vec![
            Span::styled(format!("{k:<8}"), bold(ACCENT)),
            Span::styled(v.to_string(), fg(TEXT)),
        ])
    };
    let lines = vec![
        row("/help", "Ver la ayuda"),
        row("/new", "Nueva conversación"),
        row("/clear", "Limpiar la pantalla"),
        row("/exit", "Salir de LayerAgent"),
        row("Esc", "Cancelar lo que hace"),
    ];
    f.render_widget(Paragraph::new(lines).block(panel("Accesos rápidos")), area);
}

fn cards(app: &App, f: &mut Frame, area: Rect) {
    let parts = Layout::horizontal([Constraint::Ratio(1, 3); 3])
        .spacing(1)
        .split(area);
    for (i, (title, desc, _)) in STARTERS.iter().enumerate() {
        let selected = app.card == i;
        let border = if selected {
            fg(pulse(app.clock * 0.6, ACCENT, ACCENT_LIGHT))
        } else {
            fg(BORDER)
        };
        let block = Block::bordered()
            .border_type(if selected {
                BorderType::Double
            } else {
                BorderType::Rounded
            })
            .border_style(border)
            .padding(Padding::horizontal(1));
        let inner = parts[i].width.saturating_sub(6) as usize;
        let mut lines = vec![Line::from(vec![
            Span::styled(
                format!("{} · ", i + 1),
                bold(if selected { ACCENT } else { DIM }),
            ),
            Span::styled(
                title.to_string(),
                bold(if selected { WHITE } else { ACCENT_LIGHT }),
            ),
        ])];
        lines.extend(
            wrap(desc, inner)
                .into_iter()
                .take(2)
                .map(|l| Line::from(Span::styled(l, dim()))),
        );
        f.render_widget(Paragraph::new(lines).block(block), parts[i]);
    }
}

fn tip_bar(app: &App, f: &mut Frame, area: Rect) {
    let tip = TIPS[app.tip_index()];
    let mut spans = vec![Span::styled("✦ Tip: ", bold(AMBER))];
    for word in tip.split(' ') {
        let style = if word.starts_with('/') || word.starts_with("Esc") {
            bold(ACCENT)
        } else {
            fg(TEXT)
        };
        spans.push(Span::styled(format!("{word} "), style));
    }
    f.render_widget(
        Paragraph::new(Line::from(spans)).block(
            Block::bordered()
                .border_type(BorderType::Rounded)
                .border_style(fg(Rgb(150, 115, 40))),
        ),
        area,
    );
}

/// The prompt as a single line, like a shell: `☁ LayerAgent • ~/proyecto ›`.
fn prompt_line(app: &App, f: &mut Frame, area: Rect) {
    let label = format!(
        "LayerAgent • {} › ",
        short_path(
            &app.info.path,
            (area.width as usize).saturating_sub(30).max(12)
        )
    );
    let input = app.input_text();
    let mut spans = vec![
        Span::styled("☁ ", bold(ACCENT)),
        Span::styled(label.clone(), bold(ACCENT_LIGHT)),
    ];
    if input.is_empty() {
        spans.push(Span::styled(
            "Escribe tu mensaje, o pulsa 1·2·3 para empezar…",
            fg(FAINT),
        ));
    } else {
        spans.push(Span::styled(input, fg(WHITE)));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
    if app.approval.is_none() && !app.leaving() {
        let before: String = app.input.iter().take(app.cursor).collect();
        let x = area.x + (2 + width_of(&label) + width_of(&before)) as u16;
        f.set_cursor_position((x.min(area.right().saturating_sub(1)), area.y));
    }
}

fn home(app: &App, f: &mut Frame, area: Rect, size: Layout3) {
    let (hero_h, cards_h, tip_h) = match size {
        Layout3::Wide => (23, 5, 3),
        Layout3::Medium => (18, 5, 3),
        _ => (0, 0, 0),
    };
    match size {
        Layout3::Wide | Layout3::Medium => {
            let rows = Layout::vertical([
                Constraint::Length(1),
                Constraint::Min(hero_h),
                Constraint::Length(cards_h),
                Constraint::Length(tip_h),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .split(area);
            let hero = rows[1];
            if size == Layout3::Wide {
                let cols = Layout::horizontal([
                    Constraint::Length(50),
                    Constraint::Min(60),
                    Constraint::Length(38),
                ])
                .spacing(2)
                .split(hero);
                draw_mascot(app, f, cols[0], true);
                center_column(app, f, cols[1], false);
                let right = Layout::vertical([Constraint::Length(8), Constraint::Length(7)])
                    .spacing(1)
                    .split(cols[2]);
                system_panel(app, f, right[0]);
                quick_panel(f, right[1]);
            } else {
                let cols = Layout::horizontal([Constraint::Length(38), Constraint::Min(50)])
                    .spacing(2)
                    .split(hero);
                draw_mascot(app, f, cols[0], true);
                center_column(app, f, cols[1], true);
            }
            cards(app, f, rows[2]);
            tip_bar(app, f, rows[3]);
            if size == Layout3::Medium {
                let info = format!(
                    "{} · {} · {}",
                    app.info.model,
                    app.info.project,
                    short_path(&app.info.path, 40)
                );
                f.render_widget(
                    Paragraph::new(Line::from(Span::styled(
                        truncate(&info, area.width as usize),
                        dim(),
                    ))),
                    rows[4],
                );
            }
            prompt_line(app, f, rows[5]);
        }
        Layout3::Narrow => {
            let rows = Layout::vertical([
                Constraint::Min(8),
                Constraint::Length(2),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .split(area);
            draw_mascot(app, f, rows[0], true);
            f.render_widget(
                Paragraph::new(vec![
                    Line::from(shimmer(
                        "L A Y E R A G E N T",
                        app.clock,
                        ACCENT_LIGHT,
                        WHITE,
                    )),
                    Line::from(Span::styled("Tu agente de desarrollo con IA", dim())),
                ])
                .centered(),
                rows[1],
            );
            f.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    truncate(
                        &format!("{} · {}", app.info.model, app.info.project),
                        area.width as usize,
                    ),
                    dim(),
                )))
                .centered(),
                rows[2],
            );
            prompt_line(app, f, rows[3]);
        }
        Layout3::Tiny => {
            let rows = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(area);
            f.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    "☁ LayerAgent — amplía la terminal para ver la interfaz completa",
                    dim(),
                ))),
                rows[0],
            );
            prompt_line(app, f, rows[1]);
        }
    }
}

// ───────────────────────────── conversation ─────────────────────────────

fn tool_icon(name: &str) -> &'static str {
    match name {
        "shell" | "pty_open" | "pty_write" | "pty_read" | "pty_close" => "⚙",
        "read_file" | "write_file" | "edit_file" | "list_files" => "✎",
        "search_files" | "memory_search" => "⌕",
        n if n.starts_with("browser_") => "☁",
        "spawn_agent" | "wait_agent" | "cancel_agent" | "list_agents" => "⛭",
        n if n.starts_with("git_") => "⎇",
        _ => "◈",
    }
}

fn assistant_lines(text: &str, width: usize, caret: bool, out: &mut Vec<Line<'static>>) {
    let mut in_code = false;
    let mut first = true;
    let lead = |first: bool| {
        if first {
            Span::styled("◆ ", bold(ACCENT))
        } else {
            Span::raw("  ")
        }
    };
    let raw: Vec<&str> = text.lines().collect();
    for (n, line) in raw.iter().enumerate() {
        let last = n + 1 == raw.len();
        if line.trim_start().starts_with("```") {
            in_code = !in_code;
            continue;
        }
        if in_code {
            for w in wrap(line, width.saturating_sub(5)) {
                out.push(Line::from(vec![
                    lead(first),
                    Span::styled("▏ ", fg(FAINT)),
                    Span::styled(w, fg(ACCENT_LIGHT)),
                ]));
                first = false;
            }
            continue;
        }
        let (style, body) = if let Some(h) = line
            .strip_prefix("### ")
            .or_else(|| line.strip_prefix("## "))
            .or_else(|| line.strip_prefix("# "))
        {
            (bold(ACCENT_LIGHT), h.to_string())
        } else if let Some(b) = line.strip_prefix("- ").or_else(|| line.strip_prefix("* ")) {
            (fg(TEXT), format!("• {b}"))
        } else {
            (fg(TEXT), (*line).to_string())
        };
        for (k, w) in wrap(&body, width.saturating_sub(2)).into_iter().enumerate() {
            let mut spans = vec![lead(first)];
            let mut text = inline(&w, style);
            if caret && last && k == wrap(&body, width.saturating_sub(2)).len() - 1 {
                text.push(Span::styled("▍", fg(ACCENT)));
            }
            spans.extend(text);
            out.push(Line::from(spans));
            first = false;
        }
    }
    if text.is_empty() && caret {
        out.push(Line::from(vec![lead(true), Span::styled("▍", fg(ACCENT))]));
    }
}

fn conversation(app: &App, width: usize) -> Vec<Line<'static>> {
    let mut out: Vec<Line> = Vec::new();
    let t = app.clock;
    for entry in &app.entries {
        match entry {
            Entry::User(text) => {
                out.push(Line::raw(""));
                for (i, w) in wrap(text, width.saturating_sub(2)).into_iter().enumerate() {
                    out.push(Line::from(vec![
                        Span::styled(if i == 0 { "❯ " } else { "  " }, bold(ACCENT)),
                        Span::styled(w, bold(WHITE)),
                    ]));
                }
            }
            Entry::Assistant(text) => {
                out.push(Line::raw(""));
                assistant_lines(text, width, false, &mut out);
            }
            Entry::Tool {
                name,
                summary,
                state,
                output,
                ..
            } => {
                let status = match state {
                    ToolState::Running => {
                        Span::styled(SPINNER[(t * 12.0) as usize % 10].to_string(), bold(ACCENT))
                    }
                    ToolState::Done { ok: true, secs } => {
                        Span::styled(format!("✓ {secs:.1}s"), fg(GREEN))
                    }
                    ToolState::Done { ok: false, secs } => {
                        Span::styled(format!("✗ {secs:.1}s"), fg(RED))
                    }
                };
                let room = width.saturating_sub(name.chars().count() + 14);
                out.push(Line::from(vec![
                    Span::styled(format!("  {} ", tool_icon(name)), fg(ACCENT)),
                    Span::styled(name.clone(), bold(ACCENT_LIGHT)),
                    Span::styled(format!("  {}  ", truncate(summary, room)), dim()),
                    status,
                ]));
                let keep = match state {
                    ToolState::Running => 4,
                    ToolState::Done { ok: false, .. } => 6,
                    ToolState::Done { ok: true, .. } => 0,
                };
                let start = output.len().saturating_sub(keep);
                for line in &output[start..] {
                    out.push(Line::from(vec![
                        Span::styled("    │ ", fg(FAINT)),
                        Span::styled(truncate(line, width.saturating_sub(7)), dim()),
                    ]));
                }
            }
            Entry::Changes { files, partial } => {
                let title = if *partial {
                    "archivos con cambios (turno interrumpido)"
                } else {
                    "archivos modificados"
                };
                out.push(Line::from(vec![
                    Span::styled("  ✎ ", fg(AMBER)),
                    Span::styled(format!("{} {title}", files.len()), bold(AMBER)),
                ]));
                for (kind, path) in files.iter().take(8) {
                    let (mark, color) = match kind.as_str() {
                        "added" => ("+", GREEN),
                        "deleted" => ("−", RED),
                        _ => ("~", AMBER),
                    };
                    out.push(Line::from(vec![
                        Span::styled(format!("    {mark} "), bold(color)),
                        Span::styled(truncate(path, width.saturating_sub(8)), fg(TEXT)),
                    ]));
                }
                if files.len() > 8 {
                    out.push(Line::from(Span::styled(
                        format!("    … y {} más", files.len() - 8),
                        dim(),
                    )));
                }
            }
            Entry::Agent { name, status } => {
                out.push(Line::from(vec![
                    Span::styled("  ⛭ ", fg(ACCENT)),
                    Span::styled(name.clone(), bold(ACCENT_LIGHT)),
                    Span::styled(format!(" · {status}"), dim()),
                ]));
            }
            Entry::Note(text) => out.push(Line::from(Span::styled(
                format!("  · {text}"),
                dim().add_modifier(Modifier::ITALIC),
            ))),
            Entry::Error(text) => {
                out.push(Line::raw(""));
                for (i, w) in wrap(text, width.saturating_sub(4)).into_iter().enumerate() {
                    out.push(Line::from(vec![
                        Span::styled(if i == 0 { "  ✗ " } else { "    " }, bold(RED)),
                        Span::styled(w, fg(RED)),
                    ]));
                }
            }
        }
    }
    if !app.streaming.is_empty() {
        out.push(Line::raw(""));
        assistant_lines(&app.streaming, width, true, &mut out);
    }
    out
}

fn status_line(app: &App, f: &mut Frame, area: Rect) {
    let mut spans = Vec::new();
    if app.is_busy() {
        spans.push(Span::styled(
            format!("{} ", SPINNER[(app.clock * 12.0) as usize % 10]),
            bold(ACCENT),
        ));
        spans.extend(shimmer(
            &format!("{}…", PHRASES[app.phrase_index()]),
            app.clock,
            ACCENT,
            WHITE,
        ));
        spans.push(Span::styled(
            format!("  {}s · Esc cancela", (app.clock - app.busy_since) as u32),
            dim(),
        ));
    } else {
        spans.push(Span::styled("● ", fg(GREEN)));
        spans.push(Span::styled("Listo", fg(GREEN)));
        spans.push(Span::styled(format!("  ·  {}", app.info.model), dim()));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
    if app.scroll > 0 {
        let hint = "↓ RePág/AvPág para volver al final";
        let w = width_of(hint) as u16;
        if area.width > w + 24 {
            f.render_widget(
                Paragraph::new(Line::from(Span::styled(hint, fg(AMBER)))),
                rect(area.right() - w, area.y, w, 1, area),
            );
        }
    }
}

fn prompt_box(app: &App, f: &mut Frame, area: Rect) {
    let waiting = app.approval.is_some();
    let border = if waiting {
        fg(pulse(app.clock, AMBER, Rgb(255, 220, 140)))
    } else if app.is_busy() {
        fg(BORDER)
    } else {
        fg(ACCENT)
    };
    let input = app.input_text();
    let mut spans = vec![Span::styled("› ", bold(ACCENT))];
    if input.is_empty() {
        let hint = if waiting {
            "Responde con S/Y para permitir o N para denegar"
        } else if app.is_busy() {
            "Puedes ir escribiendo mientras trabaja…"
        } else {
            "Escribe tu mensaje…"
        };
        spans.push(Span::styled(hint, fg(FAINT)));
    } else {
        spans.push(Span::styled(input, fg(WHITE)));
    }
    f.render_widget(
        Paragraph::new(Line::from(spans)).block(
            Block::bordered()
                .border_type(BorderType::Rounded)
                .border_style(border),
        ),
        area,
    );
    if !waiting && !app.leaving() {
        let before: String = app.input.iter().take(app.cursor).collect();
        let x = area.x + 3 + width_of(&before) as u16;
        f.set_cursor_position((x.min(area.right().saturating_sub(2)), area.y + 1));
    }
}

fn chat(app: &mut App, f: &mut Frame, area: Rect, size: Layout3) {
    let avatar = size != Layout3::Tiny && size != Layout3::Narrow && area.height >= 28;
    let bottom_h = if avatar { 8 } else { 5 };
    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(bottom_h),
    ])
    .split(area);
    // Header.
    let header = Line::from(vec![
        Span::styled(" ☁ LayerAgent", bold(ACCENT)),
        Span::styled(
            format!(
                "  ·  {}  ·  {}",
                app.info.project,
                short_path(
                    &app.info.path,
                    (area.width as usize).saturating_sub(40).max(10)
                )
            ),
            dim(),
        ),
    ]);
    f.render_widget(Paragraph::new(header), rows[0]);
    // Messages.
    let body = rows[1];
    let width = body.width.saturating_sub(2) as usize;
    let lines = conversation(app, width);
    let height = body.height as usize;
    let max_scroll = lines.len().saturating_sub(height);
    app.scroll = app.scroll.min(max_scroll);
    let end = lines.len() - app.scroll;
    let start = end.saturating_sub(height);
    let visible: Vec<Line> = lines[start..end].to_vec();
    f.render_widget(
        Paragraph::new(visible),
        rect(
            body.x + 1,
            body.y,
            body.width.saturating_sub(2),
            body.height,
            body,
        ),
    );
    // Bottom: avatar on the left, status and prompt on the right.
    let (avatar_area, controls) = if avatar {
        let cols = Layout::horizontal([Constraint::Length(20), Constraint::Min(20)]).split(rows[2]);
        (Some(cols[0]), cols[1])
    } else {
        (None, rows[2])
    };
    if let Some(a) = avatar_area {
        draw_mascot(app, f, rect(a.x, a.y, a.width, a.height, a), false);
    }
    let parts = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(3),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .split(controls);
    status_line(app, f, parts[0]);
    prompt_box(app, f, parts[1]);
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "Enter envía · Esc cancela · RePág/AvPág desplaza · /help ayuda",
            fg(FAINT),
        ))),
        parts[2],
    );
}

fn approval_modal(app: &mut App, f: &mut Frame, area: Rect) {
    let clock = app.clock;
    let Some(a) = &mut app.approval else { return };
    let r = Rect::new(
        area.x + 1,
        area.y + 1,
        area.width.saturating_sub(2),
        area.height.saturating_sub(2),
    );
    f.render_widget(Clear, r);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(fg(pulse(clock, AMBER, Rgb(255, 225, 150))))
        .title(Span::styled(" ¿Permitir esta acción? ", bold(AMBER)));
    let inner = block.inner(r);
    f.render_widget(block, r);
    a.review_available = inner.width >= 32 && inner.height >= 8 && a.review_error.is_none();
    if !a.review_available {
        let error = a
            .review_error
            .as_deref()
            .unwrap_or("Pantalla pequeña: amplía a 36x12. Aprobación bloqueada. [N] denegar");
        f.render_widget(
            Paragraph::new(
                wrap(error, inner.width as usize)
                    .into_iter()
                    .map(Line::raw)
                    .collect::<Vec<_>>(),
            )
            .style(fg(RED)),
            inner,
        );
        return;
    }
    let width = inner.width as usize;
    let viewport = inner.height.saturating_sub(4) as usize;
    let rows: Vec<String> = a.detail.iter().flat_map(|line| wrap(line, width)).collect();
    if a.layout_width != width {
        a.layout_width = width;
        a.scroll = 0;
        a.reviewed_through = 0;
    }
    a.viewport = viewport;
    a.total_rows = rows.len();
    a.scroll = a.scroll.min(rows.len().saturating_sub(viewport));
    let end = a.scroll.saturating_add(viewport).min(rows.len());
    if a.scroll <= a.reviewed_through {
        a.reviewed_through = a.reviewed_through.max(end);
    }
    let heading = if let Some(agent) = &a.agent {
        format!("Agente «{agent}» · {}", a.tool)
    } else {
        format!("LayerAgent · {}", a.tool)
    };
    f.render_widget(
        Paragraph::new(truncate(&heading, width)).style(fg(AMBER)),
        Rect::new(inner.x, inner.y, inner.width, 1),
    );
    let detail_area = Rect::new(inner.x, inner.y + 1, inner.width, viewport as u16);
    f.render_widget(
        Paragraph::new(
            rows[a.scroll..end]
                .iter()
                .cloned()
                .map(Line::raw)
                .collect::<Vec<_>>(),
        )
        .style(fg(WHITE)),
        detail_area,
    );
    let status = format!(
        "Líneas {}–{}/{} · faltan {} · revisadas {}",
        a.scroll + 1,
        end,
        rows.len(),
        rows.len().saturating_sub(end),
        a.reviewed_through
    );
    f.render_widget(
        Paragraph::new(truncate(&status, width)).style(fg(AMBER)),
        Rect::new(inner.x, inner.y + 1 + viewport as u16, inner.width, 1),
    );
    let navigation = "↑/↓ · RePág/AvPág · Home/End · [N] Denegar";
    f.render_widget(
        Paragraph::new(truncate(navigation, width)).style(dim()),
        Rect::new(inner.x, inner.y + 2 + viewport as u16, inner.width, 1),
    );
    let ready = a.reviewed_through >= rows.len();
    let decision = if ready {
        "[S]/[Y]/Enter Permitir · [N]/Esc Denegar"
    } else {
        "[S] bloqueado: revisa todas las líneas · [N] Denegar"
    };
    f.render_widget(
        Paragraph::new(truncate(decision, width)).style(fg(if ready { GREEN } else { AMBER })),
        Rect::new(inner.x, inner.y + 3 + viewport as u16, inner.width, 1),
    );
}

fn help_modal(f: &mut Frame, area: Rect) {
    let row = |k: &str, v: &str| {
        Line::from(vec![
            Span::styled(format!("  {k:<14}"), bold(ACCENT)),
            Span::styled(v.to_string(), fg(TEXT)),
        ])
    };
    let lines = vec![
        Line::raw(""),
        row("Enter", "Enviar el mensaje"),
        row("Esc", "Cancelar lo que el agente hace"),
        row("↑ / ↓", "Mensajes anteriores"),
        row("RePág / AvPág", "Desplazar la conversación"),
        row("Ctrl+C", "Cancelar, o salir si no hay trabajo"),
        row("/new", "Empezar una conversación nueva"),
        row("/clear", "Limpiar la pantalla"),
        row("/exit", "Salir"),
        Line::raw(""),
        Line::from(Span::styled("  Pulsa Esc para cerrar", dim())),
    ];
    let w = 60.min(area.width.saturating_sub(4));
    let h = (lines.len() + 2) as u16;
    let r = rect(
        area.x + area.width.saturating_sub(w) / 2,
        area.y + area.height.saturating_sub(h) / 2,
        w,
        h,
        area,
    );
    f.render_widget(Clear, r);
    f.render_widget(
        Paragraph::new(lines).block(panel("Ayuda").border_style(fg(ACCENT))),
        r,
    );
}

/// Draws one frame of the whole interface.
pub fn draw(app: &mut App, f: &mut Frame) {
    let area = f.area();
    let size = classify(area);
    match app.screen {
        Screen::Home => home(app, f, area, size),
        Screen::Chat => chat(app, f, area, size),
    }
    if app.help {
        help_modal(f, area);
    }
    approval_modal(app, f, area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{Info, Key};
    use ratatui::{Terminal, backend::TestBackend};
    use serde_json::json;

    fn info() -> Info {
        Info {
            model: "claude-code".into(),
            project: "example".into(),
            path: "/home/user/projects/example".into(),
            os: "Ubuntu 24.04".into(),
            shell: "zsh 5.9".into(),
            version: "0.1.0".into(),
        }
    }

    fn render(app: &mut App, w: u16, h: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).expect("terminal");
        terminal.draw(|f| draw(app, f)).expect("draw");
        let buffer = terminal.backend().buffer().clone();
        (0..h)
            .map(|y| {
                (0..w)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn app() -> App {
        theme::set_truecolor(true);
        let mut a = App::new(info());
        a.tick(0.5);
        a
    }

    #[test]
    fn home_adapts_to_every_size_without_panicking() {
        for (w, h) in [
            (200, 50),
            (160, 38),
            (120, 34),
            (110, 30),
            (90, 26),
            (70, 22),
            (50, 12),
            (20, 5),
            (1, 1),
        ] {
            let mut a = app();
            for t in [0.5, 1.7, 5.0] {
                a.tick(t);
                let _ = render(&mut a, w, h);
            }
        }
    }

    #[test]
    fn wide_home_shows_everything_the_reference_has() {
        let screen = render(&mut app(), 170, 40);
        for needle in [
            "▀",
            "Sistema",
            "Accesos rápidos",
            "example",
            "claude-code",
            "Tu agente de desarrollo con IA",
            "Desarrollar",
            "Navegar",
            "1 · Explorar",
            "✦ Tip",
            "LayerAgent •",
            "Listo para trabajar",
            "█",
        ] {
            assert!(screen.contains(needle), "missing {needle:?}\n{screen}");
        }
    }

    #[test]
    fn medium_and_narrow_keep_the_essentials() {
        let medium = render(&mut app(), 120, 34);
        assert!(
            medium.contains("█") && medium.contains("Explorar") && medium.contains("LayerAgent •")
        );
        let narrow = render(&mut app(), 80, 26);
        assert!(narrow.contains("L A Y E R A G E N T") && narrow.contains("LayerAgent •"));
    }

    #[test]
    fn conversation_view_shows_tools_streaming_and_changes() {
        let mut a = app();
        for c in "hazlo".chars() {
            a.on_key(Key::Char(c));
        }
        a.on_key(Key::Enter);
        a.on_event(&json!({"kind":"text_delta","data":{"text":"Voy a **revisar** el `README`.\n- primero\n```\ncodigo()\n```"}}));
        a.on_event(&json!({"kind":"tool_started","data":{"id":"1","name":"shell","arguments":{"command":"cargo test"}}}));
        a.on_event(
            &json!({"kind":"tool_output","data":{"call_id":"1","text":"running 3 tests\n"}}),
        );
        a.tick(3.0);
        a.on_event(&json!({"kind":"workspace_diff","data":{"changes":[{"kind":"modified","path":"src/lib.rs"}]}}));
        let screen = render(&mut a, 120, 36);
        for needle in [
            "hazlo",
            "revisar",
            "• primero",
            "codigo()",
            "shell",
            "cargo test",
            "running 3 tests",
            "1 archivos modificados",
            "src/lib.rs",
            "Esc cancela",
        ] {
            assert!(screen.contains(needle), "missing {needle:?}\n{screen}");
        }
    }

    #[test]
    fn approval_and_help_overlays_appear() {
        let mut a = app();
        a.on_event(&json!({"kind":"approval_required","data":{"approval_id":"x","call":{"name":"shell","arguments":{"command":"rm -rf build"}},"agent_name":"worker"}}));
        let screen = render(&mut a, 120, 34);
        for needle in [
            "¿Permitir esta acción?",
            "worker",
            "rm -rf build",
            "[S]",
            "[N]",
        ] {
            assert!(screen.contains(needle), "missing {needle:?}\n{screen}");
        }
        let mut b = app();
        b.help = true;
        assert!(render(&mut b, 120, 34).contains("Ayuda"));
    }

    #[test]
    fn full_approval_review_reveals_suffix_destination_and_context_through_navigation() {
        let mut a = app();
        let command = format!("{}; DELETE_REMOTE_ACCOUNT", "safe prefix ".repeat(100));
        a.on_event(&json!({"kind":"approval_required","data":{
            "approval_id":"long", "call":{"name":"mcp_call","arguments":{"command":command,"extra":"tail"}},
            "destination":{"url":"https://configured.example/mcp"},
            "action_context":{"url":"https://current.example/review","target":{"text":"Destroy account"}},
            "arguments_hash":"sha256-reviewed", "expires_at":123456
        }}));
        let first = render(&mut a, 80, 20);
        assert!(first.contains("bloqueado"));
        assert_eq!(a.on_key(Key::Char('s')), crate::app::Action::None);
        let mut observed = first;
        for _ in 0..100 {
            a.on_key(Key::PageDown);
            observed.push_str(&render(&mut a, 80, 20));
            if a.approval
                .as_ref()
                .expect("pending approval")
                .reviewed_through
                >= a.approval.as_ref().expect("pending approval").total_rows
            {
                break;
            }
        }
        for needle in [
            "DELETE_REMOTE_ACCOUNT",
            "configured.example/mcp",
            "current.example/review",
            "Destroy account",
            "sha256-reviewed",
            "123456",
            "extra",
        ] {
            assert!(observed.contains(needle), "missing {needle}");
        }
        a.on_key(Key::Home);
        render(&mut a, 80, 20);
        assert_eq!(a.approval.as_ref().expect("pending approval").scroll, 0);
        a.on_key(Key::End);
        render(&mut a, 80, 20);
        assert!(matches!(
            a.on_key(Key::Char('y')),
            crate::app::Action::Approve { allow: true, .. }
        ));
    }
    #[test]
    fn tiny_or_oversized_approval_fails_closed() {
        let mut a = app();
        a.on_event(&json!({"kind":"approval_required","data":{"approval_id":"tiny","call":{"name":"shell","arguments":{"command":"true"}}}}));
        render(&mut a, 30, 8);
        assert_eq!(a.on_key(Key::Char('s')), crate::app::Action::None);
        assert!(matches!(
            a.on_key(Key::Char('n')),
            crate::app::Action::Approve { allow: false, .. }
        ));
        a.on_event(&json!({"kind":"approval_required","data":{"approval_id":"huge","call":{"name":"shell","arguments":{"command":"x".repeat(1_048_576)}}}}));
        assert!(
            a.approval
                .as_ref()
                .expect("pending approval")
                .review_error
                .is_some()
        );
        render(&mut a, 120, 34);
        assert_eq!(a.on_key(Key::Char('s')), crate::app::Action::None);
    }
    #[test]
    fn greeting_bubble_and_farewell_bubble_are_shown() {
        let mut a = app();
        assert!(render(&mut a, 170, 40).contains("¡Hola! Soy LayerAgent"));
        a.tick(10.0);
        a.on_key(Key::Ctrl('c'));
        a.tick(10.3);
        assert!(render(&mut a, 170, 40).contains("¡Hasta pronto!"));
    }

    #[test]
    fn wrapping_handles_long_words_and_wide_characters() {
        assert_eq!(wrap("uno dos tres", 7), vec!["uno dos", "tres"]);
        assert!(wrap("aaaaaaaaaaaa", 5).iter().all(|l| l.len() <= 5));
        assert!(wrap("日本語日本語", 6).iter().all(|l| width_of(l) <= 6));
        assert_eq!(truncate("abcdefgh", 5), "abcd…");
    }
}
