//! Prints what the interface looks like as plain text (colours omitted).
//! `cargo run -p layeragent-ui --example preview -- 170 40 [chat]`
use layeragent_ui::{
    app::{App, Info, Key},
    theme, view,
};
use ratatui::{Terminal, backend::TestBackend};
use serde_json::json;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (w, h): (u16, u16) = (
        args.get(1).and_then(|a| a.parse().ok()).unwrap_or(170),
        args.get(2).and_then(|a| a.parse().ok()).unwrap_or(40),
    );
    theme::set_truecolor(true);
    let mut app = App::new(Info {
        model: "claude-code".into(),
        project: "example".into(),
        path: "/home/user/projects/example".into(),
        os: "Ubuntu 24.04".into(),
        shell: "zsh 5.9".into(),
        version: "0.1.0".into(),
    });
    app.tick(0.5);
    if args.get(3).is_some_and(|a| a == "chat") {
        for c in "Arregla el fallo del login".chars() {
            app.on_key(Key::Char(c));
        }
        app.on_key(Key::Enter);
        app.on_event(&json!({"kind":"text_delta","data":{"text":"Voy a revisar el módulo de **autenticación**.\n\n- Leo `auth.rs`\n- Ejecuto las pruebas\n\n```rust\nfn login() {}\n```"}}));
        app.on_event(&json!({"kind":"tool_started","data":{"id":"1","name":"read_file","arguments":{"path":"src/auth.rs"}}}));
        app.tick(1.0);
        app.on_event(&json!({"kind":"tool_completed","data":{"call_id":"1","result":{}}}));
        app.on_event(&json!({"kind":"tool_started","data":{"id":"2","name":"shell","arguments":{"command":"cargo test auth"}}}));
        app.on_event(&json!({"kind":"tool_output","data":{"call_id":"2","text":"running 4 tests\ntest login_ok ... ok\n"}}));
        app.tick(2.0);
    } else {
        app.tick(4.0);
    }
    let mut terminal = Terminal::new(TestBackend::new(w, h)).expect("terminal");
    terminal.draw(|f| view::draw(&mut app, f)).expect("draw");
    let buf = terminal.backend().buffer().clone();
    if let Some(path) = args.get(4) {
        write_png(path, &buf, w, h);
        return;
    }
    for y in 0..h {
        let line: String = (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect();
        println!("{}", line.trim_end());
    }
}

fn rgb(color: ratatui::style::Color, fallback: (u8, u8, u8)) -> (u8, u8, u8) {
    match color {
        ratatui::style::Color::Rgb(r, g, b) => (r, g, b),
        ratatui::style::Color::Reset => fallback,
        _ => (200, 200, 200),
    }
}

/// Cells become 8x16 blocks: half-blocks are exact, other glyphs are drawn as small bars.
fn write_png(path: &str, buf: &ratatui::buffer::Buffer, w: u16, h: u16) {
    let (cw, ch) = (8usize, 16usize);
    let bg = (44u8, 10u8, 36u8);
    let (pw, ph) = (w as usize * cw, h as usize * ch);
    let mut data = vec![0u8; pw * ph * 3];
    let mut put = |x: usize, y: usize, c: (u8, u8, u8)| {
        let i = (y * pw + x) * 3;
        data[i] = c.0;
        data[i + 1] = c.1;
        data[i + 2] = c.2;
    };
    for cy in 0..h as usize {
        for cx in 0..w as usize {
            let cell = &buf[(cx as u16, cy as u16)];
            let (f, b) = (rgb(cell.fg, (214, 221, 241)), rgb(cell.bg, bg));
            let sym = cell.symbol();
            for py in 0..ch {
                for px in 0..cw {
                    let c = match sym {
                        "▀" => {
                            if py < ch / 2 {
                                f
                            } else {
                                b
                            }
                        }
                        "▄" => {
                            if py >= ch / 2 {
                                f
                            } else {
                                b
                            }
                        }
                        "█" => f,
                        "▒" => ((f.0 + b.0) / 2, (f.1 + b.1) / 2, (f.2 + b.2) / 2),
                        " " => b,
                        _ => {
                            if (3..5).contains(&px) && (5..11).contains(&py) {
                                f
                            } else {
                                b
                            }
                        }
                    };
                    put(cx * cw + px, cy * ch + py, c);
                }
            }
        }
    }
    let file = std::fs::File::create(path).expect("create png");
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), pw as u32, ph as u32);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()
        .expect("header")
        .write_image_data(&data)
        .expect("data");
}
