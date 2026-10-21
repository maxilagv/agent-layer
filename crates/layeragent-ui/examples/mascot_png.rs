//! Writes frames of the mascot to PNG so the artwork can be reviewed outside a terminal.
//! `cargo run -p layeragent-ui --example mascot_png -- /tmp/out`
use layeragent_ui::mascot::{Pose, render};

fn write(path: &str, pose: &Pose, w: usize, h: usize, zoom: usize) {
    let canvas = render(w, h, pose);
    let (bw, bh) = (w * zoom, h * zoom);
    let mut data = vec![0u8; bw * bh * 3];
    for y in 0..bh {
        for x in 0..bw {
            let (c, a) = canvas.rgba(x / zoom, y / zoom);
            let bg = (44.0, 10.0, 36.0);
            let mix = |f: u8, b: f32| (b + (f as f32 - b) * a) as u8;
            let i = (y * bw + x) * 3;
            data[i] = mix(c.0, bg.0);
            data[i + 1] = mix(c.1, bg.1);
            data[i + 2] = mix(c.2, bg.2);
        }
    }
    let file = std::fs::File::create(path).expect("create png");
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), bw as u32, bh as u32);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()
        .expect("header")
        .write_image_data(&data)
        .expect("data");
}

fn main() {
    let dir = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/tmp/mascot".into());
    std::fs::create_dir_all(&dir).expect("dir");
    write(&format!("{dir}/idle.png"), &Pose::idle(1.0), 100, 98, 7);
    write(
        &format!("{dir}/thinking.png"),
        &Pose::thinking(2.0),
        100,
        98,
        7,
    );
    write(
        &format!("{dir}/wave.png"),
        &Pose::greeting(1.0, 0.45),
        100,
        98,
        7,
    );
    write(&format!("{dir}/small.png"), &Pose::idle(1.0), 44, 43, 14);
}
