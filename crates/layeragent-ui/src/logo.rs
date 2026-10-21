//! The LAYERAGENT wordmark in 6x6 block letters, with a drop shadow, a metallic gradient
//! and a highlight that sweeps across it every few seconds.
use crate::raster::Rgb;
use crate::theme::{self, c};
use ratatui::style::Style;
use ratatui::text::{Line, Span};

pub const HEIGHT: usize = 7;

fn glyph(ch: char) -> [&'static str; 6] {
    match ch {
        'L' => ["██    ", "██    ", "██    ", "██    ", "██    ", "██████"],
        'A' => [" ████ ", "██  ██", "██  ██", "██████", "██  ██", "██  ██"],
        'Y' => ["██  ██", "██  ██", " ████ ", "  ██  ", "  ██  ", "  ██  "],
        'E' => ["██████", "██    ", "█████ ", "██    ", "██    ", "██████"],
        'R' => ["█████ ", "██  ██", "██  ██", "█████ ", "██ ██ ", "██  ██"],
        'G' => [" █████", "██    ", "██ ███", "██  ██", "██  ██", " █████"],
        'N' => ["██  ██", "███ ██", "██████", "██ ███", "██  ██", "██  ██"],
        'T' => ["██████", "  ██  ", "  ██  ", "  ██  ", "  ██  ", "  ██  "],
        _ => ["      "; 6],
    }
}

const WORD: &str = "LAYERAGENT";

pub fn width() -> usize {
    WORD.len() * 7 + 1
}

/// `t` is seconds; the shine passes over the word once every 7 seconds.
pub fn render(t: f32) -> Vec<Line<'static>> {
    let cols = width();
    let mut on = vec![vec![false; cols]; HEIGHT];
    let mut layer = vec![0usize; cols];
    for (i, ch) in WORD.chars().enumerate() {
        for (row, bits) in glyph(ch).iter().enumerate() {
            for (k, b) in bits.chars().enumerate() {
                if b == '█' {
                    on[row][i * 7 + k] = true;
                    layer[i * 7 + k] = i;
                }
            }
        }
    }
    let sweep = (t % 7.0) / 7.0 * (cols as f32 + 30.0) - 15.0;
    let mut lines = Vec::new();
    for row in 0..HEIGHT {
        let mut spans = Vec::with_capacity(cols);
        for x in 0..cols {
            let lit = on[row][x] && row < 6;
            let shadow = x > 0 && row > 0 && on[row - 1][x - 1];
            if lit {
                let (top, bottom) = if layer[x] < 5 {
                    (theme::WHITE, Rgb(150, 168, 205))
                } else {
                    (Rgb(150, 200, 255), theme::DEEP)
                };
                let base = top.lerp(bottom, row as f32 / 5.0);
                let shine =
                    (1.0 - ((x as f32 + row as f32 * 2.0 - sweep).abs() / 5.0)).clamp(0.0, 1.0);
                spans.push(Span::styled(
                    "█",
                    Style::default().fg(c(base.lerp(Rgb(255, 255, 255), shine * 0.85))),
                ));
            } else if shadow {
                spans.push(Span::styled("▒", Style::default().fg(c(Rgb(30, 44, 110)))));
            } else {
                spans.push(Span::raw(" "));
            }
        }
        lines.push(Line::from(spans));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wordmark_has_fixed_geometry_and_a_shadow() {
        let lines = render(0.0);
        assert_eq!(lines.len(), HEIGHT);
        assert!(lines.iter().all(|l| l.spans.len() == width()));
        let text: String = lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains('█') && text.contains('▒'));
    }

    #[test]
    fn the_shine_moves() {
        let flat = |t| {
            render(t)
                .iter()
                .flat_map(|l| l.spans.iter().map(|s| format!("{:?}", s.style)))
                .collect::<Vec<_>>()
        };
        assert_ne!(flat(0.5), flat(3.0));
    }
}
