//! Palette shared by every widget. Colours degrade to xterm-256 on terminals without
//! 24-bit support.
use crate::raster::{Rgb, color};
use ratatui::style::{Color, Modifier, Style};
use std::sync::atomic::{AtomicBool, Ordering};

pub const ACCENT: Rgb = Rgb(91, 155, 255);
pub const ACCENT_LIGHT: Rgb = Rgb(156, 196, 255);
pub const DEEP: Rgb = Rgb(42, 85, 217);
pub const WHITE: Rgb = Rgb(240, 246, 255);
pub const TEXT: Rgb = Rgb(214, 221, 241);
pub const DIM: Rgb = Rgb(128, 138, 173);
pub const FAINT: Rgb = Rgb(84, 94, 134);
pub const BORDER: Rgb = Rgb(66, 84, 150);
pub const AMBER: Rgb = Rgb(232, 182, 68);
pub const GREEN: Rgb = Rgb(70, 190, 100);
pub const RED: Rgb = Rgb(245, 90, 80);
/// What transparent mascot pixels are blended against; any dark purple/blue is fine.
pub const BACKDROP: Rgb = Rgb(28, 10, 38);

static TRUECOLOR: AtomicBool = AtomicBool::new(true);

pub fn set_truecolor(on: bool) {
    TRUECOLOR.store(on, Ordering::Relaxed);
}

pub fn truecolor() -> bool {
    TRUECOLOR.load(Ordering::Relaxed)
}

pub fn c(rgb: Rgb) -> Color {
    color(rgb, truecolor())
}

pub fn fg(rgb: Rgb) -> Style {
    Style::default().fg(c(rgb))
}

pub fn bold(rgb: Rgb) -> Style {
    fg(rgb).add_modifier(Modifier::BOLD)
}

pub fn dim() -> Style {
    fg(DIM)
}
