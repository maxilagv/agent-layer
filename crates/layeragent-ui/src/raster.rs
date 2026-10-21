//! A tiny anti-aliased software rasteriser whose output is shown with half-block
//! characters, giving two square pixels per terminal cell.
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    pub fn lerp(self, other: Rgb, t: f32) -> Rgb {
        let t = t.clamp(0.0, 1.0);
        let mix = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
        Rgb(
            mix(self.0, other.0),
            mix(self.1, other.1),
            mix(self.2, other.2),
        )
    }
    pub fn scale(self, k: f32) -> Rgb {
        let f = |c: u8| (c as f32 * k).clamp(0.0, 255.0) as u8;
        Rgb(f(self.0), f(self.1), f(self.2))
    }
}

#[derive(Clone, Copy, Default)]
struct Px {
    r: f32,
    g: f32,
    b: f32,
    a: f32,
}

pub struct Canvas {
    pub w: usize,
    pub h: usize,
    px: Vec<Px>,
}

/// Signed distance (in pixels) to a shape: negative inside.
pub type Sdf<'a> = &'a dyn Fn(f32, f32) -> f32;

pub fn circle(cx: f32, cy: f32, r: f32) -> impl Fn(f32, f32) -> f32 {
    move |x, y| ((x - cx).powi(2) + (y - cy).powi(2)).sqrt() - r
}

/// Rotated ellipse (angle in radians); distance is approximate but monotone.
pub fn ellipse(cx: f32, cy: f32, rx: f32, ry: f32, angle: f32) -> impl Fn(f32, f32) -> f32 {
    let (s, c) = angle.sin_cos();
    move |x, y| {
        let (dx, dy) = (x - cx, y - cy);
        let (u, v) = (dx * c + dy * s, -dx * s + dy * c);
        let k = ((u / rx).powi(2) + (v / ry).powi(2)).sqrt();
        (k - 1.0) * rx.min(ry)
    }
}

/// Convex or concave polygon (even-odd inside test, exact edge distance).
pub fn polygon(points: Vec<(f32, f32)>) -> impl Fn(f32, f32) -> f32 {
    move |x, y| {
        let n = points.len();
        let mut d = f32::MAX;
        let mut inside = false;
        let mut j = n - 1;
        for i in 0..n {
            let (a, b) = (points[i], points[j]);
            let (ex, ey) = (b.0 - a.0, b.1 - a.1);
            let (wx, wy) = (x - a.0, y - a.1);
            let t = ((wx * ex + wy * ey) / (ex * ex + ey * ey).max(1e-6)).clamp(0.0, 1.0);
            let (px, py) = (wx - ex * t, wy - ey * t);
            d = d.min((px * px + py * py).sqrt());
            if (a.1 > y) != (b.1 > y) && x < (b.0 - a.0) * (y - a.1) / (b.1 - a.1) + a.0 {
                inside = !inside;
            }
            j = i;
        }
        if inside { -d } else { d }
    }
}

/// Thin ring around a circle.
pub fn ring(cx: f32, cy: f32, r: f32, width: f32) -> impl Fn(f32, f32) -> f32 {
    move |x, y| (((x - cx).powi(2) + (y - cy).powi(2)).sqrt() - r).abs() - width / 2.0
}

impl Canvas {
    pub fn new(w: usize, h: usize) -> Self {
        Self {
            w,
            h,
            px: vec![Px::default(); w * h],
        }
    }

    /// Paints `sdf` with `color(x, y)` at `alpha`, anti-aliased over a one pixel edge.
    pub fn fill(&mut self, sdf: Sdf, color: &dyn Fn(f32, f32) -> Rgb, alpha: f32) {
        if alpha <= 0.0 {
            return;
        }
        for y in 0..self.h {
            for x in 0..self.w {
                let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
                let coverage = (0.5 - sdf(fx, fy)).clamp(0.0, 1.0) * alpha;
                if coverage <= 0.003 {
                    continue;
                }
                let c = color(fx, fy);
                let dst = &mut self.px[y * self.w + x];
                let out_a = coverage + dst.a * (1.0 - coverage);
                let mix =
                    |s: u8, d: f32| (s as f32 * coverage + d * dst.a * (1.0 - coverage)) / out_a;
                *dst = Px {
                    r: mix(c.0, dst.r),
                    g: mix(c.1, dst.g),
                    b: mix(c.2, dst.b),
                    a: out_a,
                };
            }
        }
    }

    pub fn flat(&mut self, sdf: Sdf, color: Rgb, alpha: f32) {
        self.fill(sdf, &|_, _| color, alpha);
    }

    /// Multiplies every pixel's opacity (used for fade-outs).
    pub fn fade(&mut self, k: f32) {
        for p in &mut self.px {
            p.a *= k.clamp(0.0, 1.0);
        }
    }

    pub fn rgba(&self, x: usize, y: usize) -> (Rgb, f32) {
        let p = self.px[y * self.w + x];
        (
            Rgb(p.r.round() as u8, p.g.round() as u8, p.b.round() as u8),
            p.a,
        )
    }

    /// Two vertically stacked pixels per cell. Pixels that are mostly transparent keep the
    /// terminal's own background, so the art blends with any theme.
    pub fn to_lines(&self, backdrop: Rgb, truecolor: bool) -> Vec<Line<'static>> {
        let visible = 0.28;
        let paint = |(c, a): (Rgb, f32)| -> Rgb { backdrop.lerp(c, a.clamp(0.0, 1.0)) };
        let mut lines = Vec::new();
        for row in 0..self.h.div_ceil(2) {
            let mut spans = Vec::with_capacity(self.w);
            for x in 0..self.w {
                let top = self.rgba(x, row * 2);
                let bottom = if row * 2 + 1 < self.h {
                    self.rgba(x, row * 2 + 1)
                } else {
                    (Rgb(0, 0, 0), 0.0)
                };
                let (tv, bv) = (top.1 >= visible, bottom.1 >= visible);
                let span = match (tv, bv) {
                    (true, true) => Span::styled(
                        "▀",
                        Style::default()
                            .fg(color(paint(top), truecolor))
                            .bg(color(paint(bottom), truecolor)),
                    ),
                    (true, false) => {
                        Span::styled("▀", Style::default().fg(color(paint(top), truecolor)))
                    }
                    (false, true) => {
                        Span::styled("▄", Style::default().fg(color(paint(bottom), truecolor)))
                    }
                    (false, false) => Span::raw(" "),
                };
                spans.push(span);
            }
            lines.push(Line::from(spans));
        }
        lines
    }
}

/// Terminal colour: 24-bit when supported, otherwise the nearest xterm-256 entry.
pub fn color(c: Rgb, truecolor: bool) -> Color {
    if truecolor {
        return Color::Rgb(c.0, c.1, c.2);
    }
    let level = |v: u8| -> u8 {
        if v < 48 {
            0
        } else if v < 115 {
            1
        } else {
            (v - 35) / 40
        }
    };
    let (r, g, b) = (level(c.0), level(c.1), level(c.2));
    let cube = 16 + 36 * r + 6 * g + b;
    let grey = |v: u8| -> u8 {
        if v > 238 {
            23
        } else {
            (v.saturating_sub(3)) / 10
        }
    };
    let avg = ((c.0 as u16 + c.1 as u16 + c.2 as u16) / 3) as u8;
    let spread = c.0.max(c.1).max(c.2) - c.0.min(c.1).min(c.2);
    if spread < 10 {
        Color::Indexed(232 + grey(avg))
    } else {
        Color::Indexed(cube)
    }
}

pub fn truecolor_supported() -> bool {
    std::env::var("COLORTERM").is_ok_and(|v| v.contains("truecolor") || v.contains("24bit"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shapes_cover_their_interior_and_not_far_outside() {
        let mut canvas = Canvas::new(20, 20);
        canvas.flat(&circle(10.0, 10.0, 4.9), Rgb(255, 0, 0), 1.0);
        assert!(canvas.rgba(10, 10).1 > 0.99);
        assert!(canvas.rgba(1, 1).1 < 0.01);
        let edge = canvas.rgba(14, 10).1;
        assert!(edge > 0.0 && edge < 1.0, "edge is anti-aliased: {edge}");
    }

    #[test]
    fn polygon_and_ellipse_have_correct_inside_tests() {
        let tri = polygon(vec![(0.0, 0.0), (10.0, 0.0), (5.0, 10.0)]);
        assert!(tri(5.0, 3.0) < 0.0 && tri(0.5, 9.0) > 0.0);
        let e = ellipse(10.0, 10.0, 8.0, 3.0, 0.0);
        assert!(e(10.0, 10.0) < 0.0 && e(10.0, 15.0) > 0.0);
    }

    #[test]
    fn half_blocks_pair_two_pixels_and_transparent_cells_are_blank() {
        let mut canvas = Canvas::new(2, 4);
        canvas.flat(
            &|x, y| if x < 1.0 && y < 2.0 { -1.0 } else { 1.0 },
            Rgb(10, 20, 30),
            1.0,
        );
        let lines = canvas.to_lines(Rgb(0, 0, 0), true);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].spans[0].content, "▀");
        assert_eq!(lines[0].spans[1].content, " ");
        assert_eq!(lines[1].spans[0].content, " ");
    }

    #[test]
    fn indexed_fallback_maps_greys_and_hues() {
        assert!(matches!(
            color(Rgb(128, 128, 128), false),
            Color::Indexed(232..=255)
        ));
        assert!(matches!(color(Rgb(255, 0, 0), false), Color::Indexed(196)));
    }
}
