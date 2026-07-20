//! Software preview of the overlay: renders the HUD to a PNG with no graphics API involved.
//!
//! Useful for checking layout, font and colours by eye without bringing up D3D11 or Vulkan,
//! and for having something to compare against when the on-screen result looks wrong. A
//! debugging tool; it is not part of any release.
//!
//! ```sh
//! cargo run -p bs-render --example preview
//! ```

use bs_core::{CoreMetrics, FrameMetrics, GpuMetrics, MetricsSnapshot, Power, Theme, Vendor};
use bs_render::{DrawList, GlyphAtlas, HudOptions, hud};

const FONT_PX: f32 = 16.0;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let atlas = GlyphAtlas::new(bs_render::EMBEDDED_FONT, FONT_PX)?;
    let theme = Theme::default();
    let opts = HudOptions::default();

    // Two states side by side: an empty snapshot (how the overlay looks before the first
    // sample) and a populated one. The empty case matters more — it shows that unreadable
    // metrics come out as dashes.
    for (name, snapshot) in [
        ("empty", MetricsSnapshot::default()),
        ("populated", populated()),
    ] {
        let (list, size) = hud::build(&atlas, &snapshot, &theme, &opts);
        let (w, h) = (size.width.ceil() as usize, size.height.ceil() as usize);
        let pixels = rasterize(&list, &atlas, w, h);

        let path = format!("target/preview-{name}.png");
        write_png(&path, &pixels, w, h)?;
        println!("{path}  {w}x{h}, quads: {}", list.indices.len() / 6);
    }

    let path = "target/preview-atlas.png";
    write_gray_png(
        path,
        &atlas.pixels,
        atlas.width as usize,
        atlas.height as usize,
    )?;
    println!("{path}  {}x{}", atlas.width, atlas.height);
    Ok(())
}

/// Rasterises the draw list to RGBA8 on the CPU.
///
/// Does exactly what the real overlay shader does: no filtering, coverage sampled from the
/// atlas, premultiplied colour blended over what is already there.
fn rasterize(list: &DrawList, atlas: &GlyphAtlas, w: usize, h: usize) -> Vec<u8> {
    let mut buf = vec![0u8; w * h * 4];

    for quad in list.indices.chunks_exact(6) {
        let v = [
            &list.vertices[quad[0] as usize],
            &list.vertices[quad[1] as usize],
            &list.vertices[quad[2] as usize],
        ];
        let (x0, y0) = (v[0].pos[0], v[0].pos[1]);
        let (x1, y1) = (v[2].pos[0], v[2].pos[1]);
        let (u0, vv0) = (v[0].uv[0], v[0].uv[1]);
        let (u1, vv1) = (v[2].uv[0], v[2].uv[1]);
        let color = v[0].color;

        let (px0, py0) = (x0.floor().max(0.0) as usize, y0.floor().max(0.0) as usize);
        let (px1, py1) = ((x1.ceil() as usize).min(w), (y1.ceil() as usize).min(h));

        for py in py0..py1 {
            for px in px0..px1 {
                let fx = (px as f32 + 0.5 - x0) / (x1 - x0).max(f32::EPSILON);
                let fy = (py as f32 + 0.5 - y0) / (y1 - y0).max(f32::EPSILON);
                if !(0.0..1.0).contains(&fx) || !(0.0..1.0).contains(&fy) {
                    continue;
                }

                let coverage = if u0 == u1 {
                    1.0 // solid fill: the opaque texel
                } else {
                    let tx = ((u0 + (u1 - u0) * fx) * atlas.width as f32) as usize;
                    let ty = ((vv0 + (vv1 - vv0) * fy) * atlas.height as f32) as usize;
                    atlas
                        .pixels
                        .get(ty * atlas.width as usize + tx)
                        .map_or(0.0, |&c| c as f32 / 255.0)
                };

                let src = [
                    color[0] * coverage,
                    color[1] * coverage,
                    color[2] * coverage,
                    color[3] * coverage,
                ];
                let o = (py * w + px) * 4;
                for ch in 0..4 {
                    let dst = buf[o + ch] as f32 / 255.0;
                    let out = src[ch] + dst * (1.0 - src[3]);
                    buf[o + ch] = (out.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
                }
            }
        }
    }
    buf
}

fn populated() -> MetricsSnapshot {
    let mut s = MetricsSnapshot::default();
    s.cpu.name = Some("AMD Ryzen 7 7800X3D 8-Core Processor".into());
    s.cpu.load_pct = Some(42.0);
    s.cpu.power = Some(Power::Estimated(65.0));
    s.cpu.cores = (0..16)
        .map(|i| CoreMetrics {
            load_pct: [12.0, 88.0, 34.0, 95.0][i % 4],
            freq_mhz: Some(4200.0 + i as f32 * 40.0),
        })
        .collect();
    s.gpu = GpuMetrics {
        name: Some("NVIDIA GeForce RTX 4070".into()),
        vendor: Vendor::Nvidia,
        load_pct: Some(88.0),
        vram_used_bytes: Some(6_500_000_000),
        vram_total_bytes: Some(12_884_901_888),
        temp_c: Some(62.0),
        core_clock_mhz: Some(2610.0),
        power: Some(Power::Measured(145.0)),
    };
    s.memory.used_bytes = Some(19_000_000_000);
    s.memory.total_bytes = Some(34_359_738_368);
    s.memory.speed_mhz = Some(6000);
    s.frames = Some(FrameMetrics {
        fps: 144.0,
        frametime_ms: 6.9,
        avg_fps: 141.0,
        low_1pct: Some(98.0),
        low_01pct: Some(72.0),
        sample_count: 2000,
    });
    s
}

fn write_png(
    path: &str,
    rgba: &[u8],
    w: usize,
    h: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    // The overlay is semi-transparent, so a checkerboard goes underneath it — against a white
    // viewer background there would be no telling transparency from pale text.
    let mut composited = vec![0u8; w * h * 4];
    for y in 0..h {
        for x in 0..w {
            let o = (y * w + x) * 4;
            let checker = if (x / 8 + y / 8) % 2 == 0 {
                0x50u8
            } else {
                0x38
            };
            let a = rgba[o + 3] as f32 / 255.0;
            for ch in 0..3 {
                // The colour is already premultiplied, so the background mixes in via (1 - a).
                composited[o + ch] =
                    (rgba[o + ch] as f32 + checker as f32 * (1.0 - a)).min(255.0) as u8;
            }
            composited[o + 3] = 0xFF;
        }
    }
    encode(path, &composited, w, h, png::ColorType::Rgba)
}

fn write_gray_png(
    path: &str,
    gray: &[u8],
    w: usize,
    h: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    encode(path, gray, w, h, png::ColorType::Grayscale)
}

fn encode(
    path: &str,
    data: &[u8],
    w: usize,
    h: usize,
    color: png::ColorType,
) -> Result<(), Box<dyn std::error::Error>> {
    let file = std::fs::File::create(path)?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w as u32, h as u32);
    enc.set_color(color);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()?.write_image_data(data)?;
    Ok(())
}
