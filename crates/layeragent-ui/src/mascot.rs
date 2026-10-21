//! The LayerAgent mascot: a hooded spirit with feathered wings, meditating on a cloud.
//!
//! It is drawn procedurally in a 100x98 design space and scaled to whatever pixel size is
//! asked for, so every animation (floating, wing flaps, blinking, waving, orbiting lights,
//! fading out) is just a different [`Pose`].
use crate::raster::{Canvas, Rgb, circle, ellipse, polygon, ring};

const NAVY_DARK: Rgb = Rgb(9, 17, 46);
const NAVY: Rgb = Rgb(18, 33, 79);
const NAVY_LIGHT: Rgb = Rgb(36, 60, 128);
const RIM: Rgb = Rgb(111, 163, 255);
const RIM_BRIGHT: Rgb = Rgb(190, 215, 255);
const FEATHER_LIGHT: Rgb = Rgb(232, 241, 255);
const FEATHER_DARK: Rgb = Rgb(62, 100, 205);
const CLOUD_LIGHT: Rgb = Rgb(214, 228, 255);
const CLOUD_MID: Rgb = Rgb(112, 156, 245);
const CLOUD_DARK: Rgb = Rgb(44, 78, 181);
const CLOUD_OUTLINE: Rgb = Rgb(24, 40, 118);
const EYE: Rgb = Rgb(236, 247, 255);
const EYE_GLOW: Rgb = Rgb(74, 140, 255);
const WING_EDGE: Rgb = Rgb(48, 84, 196);
const FACE: Rgb = Rgb(3, 5, 14);
const HAND: Rgb = Rgb(205, 222, 255);

/// Height of the design space; the cloud's lowest puff reaches about y = 96.
const DESIGN_H: f32 = 98.0;

/// Everything that can change from frame to frame.
#[derive(Clone, Copy, Debug)]
pub struct Pose {
    /// Seconds since the animation began; drives twinkling and orbiting.
    pub t: f32,
    /// Wing position, -1 (lowered) ..= 1 (raised).
    pub flap: f32,
    /// Vertical float offset in design units.
    pub bob: f32,
    /// 0 = hands folded, 1 = right hand raised high (swings while waving).
    pub wave: f32,
    /// Eye opening, 0 (closed) ..= 1.
    pub eyes: f32,
    /// Glow intensity of eyes and halo.
    pub glow: f32,
    /// 0 = serene, 1 = concentrating: halo spins and lights orbit the head.
    pub think: f32,
    /// Overall opacity.
    pub fade: f32,
    /// Downward shift of the whole figure, in design units.
    pub sink: f32,
}

impl Pose {
    pub fn idle(t: f32) -> Self {
        let blink_phase = (t % 4.6) / 0.16;
        let eyes = if blink_phase < 1.0 {
            (blink_phase * std::f32::consts::PI).cos().abs()
        } else {
            1.0
        };
        Self {
            t,
            flap: 0.22 * (t * 1.1 + 0.6).sin(),
            bob: 1.7 * (t * 1.1).sin(),
            wave: 0.0,
            eyes,
            glow: 0.7 + 0.2 * (t * 1.7).sin(),
            think: 0.0,
            fade: 1.0,
            sink: 0.0,
        }
    }

    pub fn thinking(t: f32) -> Self {
        Self {
            flap: 0.35 * (t * 1.6).sin(),
            bob: 2.2 * (t * 1.5).sin(),
            glow: 0.9 + 0.1 * (t * 5.0).sin(),
            think: 1.0,
            eyes: 0.85 + 0.15 * (t * 3.0).sin(),
            ..Self::idle(t)
        }
    }

    /// `p` runs from 0 to 1 over the greeting: the wings open and a hand waves.
    pub fn greeting(t: f32, p: f32) -> Self {
        let up = (p / 0.18).clamp(0.0, 1.0);
        let down = ((1.0 - p) / 0.2).clamp(0.0, 1.0);
        let envelope = up.min(down);
        Self {
            wave: envelope,
            flap: 0.9 * (p * std::f32::consts::PI).sin() + 0.1 * (t * 6.0).sin(),
            glow: 1.0,
            eyes: 1.0,
            ..Self::idle(t)
        }
    }

    /// `p` runs from 0 to 1: a last wave, then the spirit sinks into the cloud and fades.
    pub fn farewell(t: f32, p: f32) -> Self {
        let wave = ((p / 0.15).clamp(0.0, 1.0)).min(((0.62 - p) / 0.12).clamp(0.0, 1.0));
        let leaving = ((p - 0.55) / 0.45).clamp(0.0, 1.0);
        Self {
            wave,
            flap: 0.5 * (p * 9.0).sin() - leaving * 0.8,
            sink: leaving * 16.0,
            fade: 1.0 - leaving.powf(1.4),
            glow: 1.0 - leaving * 0.6,
            eyes: if leaving > 0.6 { 0.3 } else { 1.0 },
            ..Self::idle(t)
        }
    }
}

struct Pen {
    c: Canvas,
    s: f32,
    ox: f32,
    oy: f32,
}

impl Pen {
    fn x(&self, v: f32) -> f32 {
        self.ox + v * self.s
    }
    fn y(&self, v: f32) -> f32 {
        self.oy + v * self.s
    }
    fn circ(&mut self, cx: f32, cy: f32, r: f32, color: Rgb, alpha: f32) {
        let sdf = circle(self.x(cx), self.y(cy), r * self.s);
        self.c.flat(&sdf, color, alpha);
    }

    fn circ_grad(&mut self, cx: f32, cy: f32, r: f32, top: Rgb, bottom: Rgb, alpha: f32) {
        let sdf = circle(self.x(cx), self.y(cy), r * self.s);
        let (oy, s) = (self.oy, self.s);
        let (y0, h) = (cy - r, 2.0 * r);
        self.c.fill(
            &sdf,
            &move |_, py| top.lerp(bottom, ((py - oy) / s - y0) / h),
            alpha,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn ell(&mut self, cx: f32, cy: f32, rx: f32, ry: f32, angle: f32, color: Rgb, alpha: f32) {
        let sdf = ellipse(self.x(cx), self.y(cy), rx * self.s, ry * self.s, angle);
        self.c.flat(&sdf, color, alpha);
    }

    #[allow(clippy::too_many_arguments)]
    fn ell_grad(&mut self, cx: f32, cy: f32, rx: f32, ry: f32, top: Rgb, bottom: Rgb, alpha: f32) {
        let sdf = ellipse(self.x(cx), self.y(cy), rx * self.s, ry * self.s, 0.0);
        let (oy, s) = (self.oy, self.s);
        let (y0, h) = (cy - ry, 2.0 * ry);
        self.c.fill(
            &sdf,
            &move |_, py| top.lerp(bottom, ((py - oy) / s - y0) / h),
            alpha,
        );
    }

    fn poly(&mut self, pts: &[(f32, f32)], color: Rgb, alpha: f32) {
        let sdf = polygon(pts.iter().map(|&(x, y)| (self.x(x), self.y(y))).collect());
        self.c.flat(&sdf, color, alpha);
    }

    fn poly_grad(&mut self, pts: &[(f32, f32)], top: Rgb, bottom: Rgb, alpha: f32) {
        let sdf = polygon(pts.iter().map(|&(x, y)| (self.x(x), self.y(y))).collect());
        let (y0, y1) = pts
            .iter()
            .fold((f32::MAX, f32::MIN), |(a, b), p| (a.min(p.1), b.max(p.1)));
        let (oy, s) = (self.oy, self.s);
        self.c.fill(
            &sdf,
            &move |_, py| top.lerp(bottom, ((py - oy) / s - y0) / (y1 - y0).max(1e-3)),
            alpha,
        );
    }
}

/// A leaf-shaped feather from `anchor` pointing `deg` degrees (y axis down).
fn feather(anchor: (f32, f32), deg: f32, len: f32, width: f32, mirror: bool) -> Vec<(f32, f32)> {
    let a = deg.to_radians();
    let (dir, perp) = ((a.cos(), a.sin()), (-a.sin(), a.cos()));
    let at = |along: f32, side: f32| {
        let (x, y) = (
            anchor.0 + dir.0 * along + perp.0 * side,
            anchor.1 + dir.1 * along + perp.1 * side,
        );
        if mirror { (100.0 - x, y) } else { (x, y) }
    };
    vec![
        at(0.0, width * 0.25),
        at(len * 0.35, width * 0.52),
        at(len * 0.75, width * 0.44),
        at(len, 0.0),
        at(len * 0.75, -width * 0.44),
        at(len * 0.35, -width * 0.52),
        at(0.0, -width * 0.25),
    ]
}

fn wings(p: &mut Pen, pose: &Pose, dy: f32) {
    // (angle, length) from the lowest to the highest feather.
    const FAN: [(f32, f32); 8] = [
        (172.0, 26.0),
        (183.0, 33.0),
        (194.0, 40.0),
        (206.0, 45.0),
        (218.0, 49.0),
        (230.0, 51.0),
        (242.0, 49.0),
        (254.0, 42.0),
    ];
    for mirror in [false, true] {
        let anchor = (42.0, 47.0 + dy);
        for (angle, len) in FAN {
            let eff = angle + pose.flap * 11.0;
            let outline = feather(anchor, eff, len + 1.7, 12.2, mirror);
            p.poly(&outline, WING_EDGE, 0.95);
            let body = feather(anchor, eff, len, 10.0, mirror);
            let dir = (eff.to_radians().cos(), eff.to_radians().sin());
            let (a, m) = (anchor, mirror);
            let sdf = polygon(body.iter().map(|&(x, y)| (p.x(x), p.y(y))).collect());
            let (ox, oy, s) = (p.ox, p.oy, p.s);
            p.c.fill(
                &sdf,
                &move |px, py| {
                    let (mut x, y) = ((px - ox) / s, (py - oy) / s);
                    if m {
                        x = 100.0 - x;
                    }
                    let t = (((x - a.0) * dir.0 + (y - a.1) * dir.1) / len).clamp(0.0, 1.0);
                    FEATHER_DARK.lerp(FEATHER_LIGHT, t * 1.15)
                },
                1.0,
            );
            // A pale quill down the middle of each feather.
            let quill = feather(anchor, eff, len * 0.88, 0.8, mirror);
            p.poly(&quill, Rgb(255, 255, 255), 0.55);
        }
    }
}

fn cloud(p: &mut Pen, t: f32, front: bool) {
    const PUFFS: [(f32, f32, f32); 13] = [
        (10.0, 80.0, 7.0),
        (16.0, 74.0, 9.0),
        (30.0, 70.0, 10.0),
        (46.0, 66.5, 11.0),
        (62.0, 67.5, 10.5),
        (77.0, 70.0, 10.0),
        (88.0, 75.0, 8.0),
        (24.0, 80.5, 10.0),
        (40.0, 82.5, 11.0),
        (58.0, 83.0, 11.0),
        (74.0, 81.0, 10.0),
        (86.0, 80.0, 7.5),
        (50.0, 86.0, 9.0),
    ];
    let grown = |i: usize, r: f32| r * (1.0 + 0.025 * (t * 1.4 + i as f32 * 0.9).sin());
    let selected = |cy: f32| (cy > 76.0) == front;
    // One silhouette outline for the whole bank, so puffs read as a single cloud.
    for (i, &(cx, cy, r)) in PUFFS.iter().enumerate().filter(|(_, p)| selected(p.1)) {
        p.circ(cx, cy, grown(i, r) + 1.4, CLOUD_OUTLINE, 0.95);
    }
    for (i, &(cx, cy, r)) in PUFFS.iter().enumerate().filter(|(_, p)| selected(p.1)) {
        let r = grown(i, r);
        p.circ_grad(cx, cy, r, CLOUD_LIGHT, CLOUD_DARK, 1.0);
        p.circ_grad(cx, cy + r * 0.3, r * 0.8, CLOUD_MID, CLOUD_DARK, 0.30);
        p.circ(
            cx - r * 0.32,
            cy - r * 0.42,
            r * 0.30,
            Rgb(255, 255, 255),
            0.28,
        );
    }
}

fn sparkle(p: &mut Pen, x: f32, y: f32, size: f32, brightness: f32) {
    let b = brightness.clamp(0.0, 1.0);
    p.circ(x, y, size * 2.4, EYE_GLOW, 0.14 * b);
    p.ell(x, y, size * 3.2, size * 0.42, 0.0, RIM_BRIGHT, 0.9 * b);
    p.ell(x, y, size * 0.42, size * 3.2, 0.0, RIM_BRIGHT, 0.9 * b);
    p.circ(x, y, size * 0.6, Rgb(255, 255, 255), b);
}

pub fn render(width: usize, height: usize, pose: &Pose) -> Canvas {
    let s = (width as f32 / 100.0).min(height as f32 / DESIGN_H);
    let mut p = Pen {
        c: Canvas::new(width, height),
        s,
        ox: (width as f32 - 100.0 * s) / 2.0,
        oy: (height as f32 - DESIGN_H * s) / 2.0,
    };
    let dy = pose.bob + pose.sink;
    let t = pose.t;

    // Halo: soft glow plus a thin ring that spins into dashes while concentrating.
    for (i, r) in [38.0, 33.0, 28.0].into_iter().enumerate() {
        p.circ(
            50.0,
            29.0 + dy,
            r,
            EYE_GLOW,
            (0.035 + 0.02 * i as f32) * pose.glow,
        );
    }
    let ring_sdf = ring(p.x(50.0), p.y(29.0 + dy), 35.5 * s, (0.9 * s).max(1.0));
    let spin = t * 1.1;
    let think = pose.think;
    let centre = (p.x(50.0), p.y(29.0 + dy));
    p.c.fill(
        &ring_sdf,
        &move |px, py| {
            let angle = (py - centre.1).atan2(px - centre.0) + spin;
            let dash = ((angle * 6.0 / std::f32::consts::PI).rem_euclid(2.0) < 1.3) as u8 as f32;
            RIM.lerp(RIM_BRIGHT, 0.5 * dash)
        },
        0.30 + 0.35 * think,
    );

    wings(&mut p, pose, dy);
    cloud(&mut p, t, false);

    // Robe and mantle.
    let robe = [
        (38.0, 42.0 + dy),
        (62.0, 42.0 + dy),
        (70.0, 52.0 + dy),
        (75.0, 64.0 + dy),
        (25.0, 64.0 + dy),
        (30.0, 52.0 + dy),
    ];
    let rim_robe: Vec<_> = robe
        .iter()
        .map(|&(x, y)| (50.0 + (x - 50.0) * 1.03, y - 0.9))
        .collect();
    p.poly_grad(&rim_robe, RIM, NAVY, 0.85);
    p.poly_grad(&robe, NAVY_LIGHT, NAVY_DARK, 1.0);
    p.ell_grad(50.0, 64.0 + dy, 25.5, 8.5, NAVY, NAVY_DARK, 1.0);
    for x in [43.0, 50.0, 57.0] {
        p.poly(
            &[
                (x - 0.5, 53.0 + dy),
                (x + 0.5, 53.0 + dy),
                (x + 1.4, 65.0 + dy),
                (x - 1.4, 65.0 + dy),
            ],
            NAVY_DARK,
            0.55,
        );
    }
    p.ell(50.0, 43.0 + dy, 15.0, 5.5, 0.0, RIM, 0.55);
    p.ell_grad(50.0, 43.4 + dy, 14.4, 5.0, NAVY_LIGHT, NAVY, 1.0);

    // Arms: the left rests folded, the right can wave.
    let chest_right = (54.0, 53.5 + dy);
    let raised = (74.0 + 4.0 * (t * 9.0).sin() * pose.wave, 33.0 + dy);
    let hand = (
        chest_right.0 + (raised.0 - chest_right.0) * pose.wave,
        chest_right.1 + (raised.1 - chest_right.1) * pose.wave,
    );
    for (shoulder, end) in [
        ((40.0, 46.0 + dy), (46.0, 53.5 + dy)),
        ((60.0, 46.0 + dy), hand),
    ] {
        let (dx, dyv) = (end.0 - shoulder.0, end.1 - shoulder.1);
        let n = (dx * dx + dyv * dyv).sqrt().max(1e-3);
        let (nx, ny) = (-dyv / n * 3.4, dx / n * 3.4);
        let quad = [
            (shoulder.0 + nx, shoulder.1 + ny),
            (end.0 + nx * 0.8, end.1 + ny * 0.8),
            (end.0 - nx * 0.8, end.1 - ny * 0.8),
            (shoulder.0 - nx, shoulder.1 - ny),
        ];
        let wide: Vec<_> = quad
            .iter()
            .map(|&(x, y)| {
                (
                    shoulder.0 + (x - shoulder.0) * 1.25,
                    shoulder.1 + (y - shoulder.1) * 1.25,
                )
            })
            .collect();
        p.poly(&wide, RIM, 0.8);
        p.poly_grad(&quad, NAVY_LIGHT, NAVY, 1.0);
        p.circ(end.0, end.1, 2.8, RIM, 0.7);
        p.circ(end.0, end.1, 2.3, HAND, 1.0);
    }

    // Hood.
    let hood_dy = dy;
    p.ell(50.0, 28.5 + hood_dy, 17.7, 19.2, 0.0, RIM, 0.9);
    p.poly(
        &[
            (50.0, 3.6 + hood_dy),
            (37.4, 20.0 + hood_dy),
            (62.6, 20.0 + hood_dy),
        ],
        RIM,
        0.9,
    );
    p.ell_grad(50.0, 29.0 + hood_dy, 16.5, 18.2, NAVY_LIGHT, NAVY_DARK, 1.0);
    p.poly_grad(
        &[
            (50.0, 5.0 + hood_dy),
            (38.4, 21.0 + hood_dy),
            (61.6, 21.0 + hood_dy),
        ],
        NAVY_LIGHT,
        NAVY,
        1.0,
    );
    p.poly_grad(
        &[
            (34.5, 30.0 + hood_dy),
            (65.5, 30.0 + hood_dy),
            (67.0, 44.0 + hood_dy),
            (33.0, 44.0 + hood_dy),
        ],
        NAVY,
        NAVY_DARK,
        1.0,
    );
    p.ell(50.0, 32.0 + hood_dy, 11.6, 12.6, 0.0, RIM, 0.65);
    p.ell_grad(50.0, 32.2 + hood_dy, 10.7, 11.7, Rgb(8, 12, 30), FACE, 1.0);

    // Eyes.
    let open = pose.eyes.clamp(0.0, 1.0);
    for (side, ex) in [(1.0_f32, 45.0_f32), (-1.0, 55.0)] {
        let angle = 0.26 * side;
        p.circ(ex, 33.0 + hood_dy, 6.2, EYE_GLOW, 0.22 * pose.glow);
        p.circ(ex, 33.0 + hood_dy, 4.2, EYE_GLOW, 0.30 * pose.glow);
        p.ell(
            ex,
            33.0 + hood_dy,
            3.8,
            (1.95 * open).max(0.25),
            angle,
            EYE_GLOW,
            0.9,
        );
        p.ell(
            ex,
            33.0 + hood_dy,
            3.2,
            (1.45 * open).max(0.2),
            angle,
            EYE,
            1.0,
        );
    }

    cloud(&mut p, t, true);

    // Twinkling stars, and lights that orbit the head while thinking.
    for (i, &(x, y, size)) in [
        (8.0, 24.0, 1.0),
        (92.0, 18.0, 1.3),
        (14.0, 52.0, 0.8),
        (90.0, 50.0, 1.0),
        (30.0, 6.0, 0.7),
        (72.0, 5.0, 0.8),
        (50.0, 1.5, 1.0),
    ]
    .iter()
    .enumerate()
    {
        let b = 0.4 + 0.6 * (0.5 + 0.5 * (t * 2.2 + i as f32 * 1.7).sin());
        sparkle(&mut p, x, y + dy * 0.3, size, b);
    }
    if pose.think > 0.01 {
        for k in 0..3 {
            for trail in 0..5 {
                let a = t * 1.9 + k as f32 * std::f32::consts::TAU / 3.0 - trail as f32 * 0.16;
                let (x, y) = (50.0 + 41.0 * a.cos(), 29.0 + dy + 33.0 * a.sin());
                // Lights fade as they swing in front of the robe, so only the upper arc reads clearly.
                let front = ((1.0 - a.sin()) / 2.0).clamp(0.0, 1.0).powf(1.5);
                p.circ(
                    x,
                    y,
                    1.9 - trail as f32 * 0.25,
                    RIM_BRIGHT,
                    pose.think * (0.95 - trail as f32 * 0.18) * front,
                );
            }
        }
    }
    let mut canvas = p.c;
    canvas.fade(pose.fade);
    let _ = (p.s, p.ox, p.oy);
    canvas
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lit(c: &Canvas) -> usize {
        (0..c.h)
            .flat_map(|y| (0..c.w).map(move |x| (x, y)))
            .filter(|&(x, y)| c.rgba(x, y).1 > 0.3)
            .count()
    }

    #[test]
    fn the_mascot_draws_and_stays_inside_the_canvas() {
        for (w, h) in [(50, 45), (30, 27), (16, 14)] {
            let c = render(w, h, &Pose::idle(0.3));
            assert!(lit(&c) > w * h / 5, "{w}x{h} looks empty");
        }
    }

    #[test]
    fn animation_changes_pixels_but_a_faded_figure_disappears() {
        let a = render(40, 36, &Pose::idle(0.0));
        let b = render(40, 36, &Pose::idle(1.4));
        let differing = (0..36)
            .flat_map(|y| (0..40).map(move |x| (x, y)))
            .filter(|&(x, y)| a.rgba(x, y).0 != b.rgba(x, y).0)
            .count();
        assert!(
            differing > 50,
            "bobbing and twinkling must move pixels: {differing}"
        );
        let gone = render(
            40,
            36,
            &Pose {
                fade: 0.0,
                ..Pose::idle(0.0)
            },
        );
        assert_eq!(lit(&gone), 0);
    }

    #[test]
    fn greeting_raises_the_hand_and_farewell_ends_invisible() {
        let rest = Pose::greeting(1.0, 0.0);
        let mid = Pose::greeting(1.0, 0.5);
        assert!(rest.wave < 0.01 && mid.wave > 0.99);
        let end = Pose::farewell(2.0, 1.0);
        assert!(end.fade < 0.01 && end.sink > 15.0);
    }

    #[test]
    fn blinking_closes_the_eyes_briefly() {
        assert!(Pose::idle(0.08).eyes < 0.1, "mid-blink the eyes are closed");
        assert!(Pose::idle(2.0).eyes > 0.99);
    }
}
