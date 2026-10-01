use super::*;
use filmcraft_geom::{Affine, Vec2};
use filmcraft_render::plan::execute_cpu;

fn device() -> Option<(wgpu::Device, wgpu::Queue)> {
    let instance = wgpu::Instance::default();
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())).ok()?;
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()
}

fn yuv_frame(w: u32, h: u32) -> Arc<VideoFrame> {
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let y: Vec<u8> = (0..w * h).map(|i| (16 + ((i % w) * 219 / w)) as u8).collect();
    let u: Vec<u8> = (0..cw * ch).map(|i| (64 + (i / cw) * 128 / ch) as u8).collect();
    let v: Vec<u8> = (0..cw * ch).map(|i| (200 - (i % cw) * 100 / cw) as u8).collect();
    Arc::new(VideoFrame {
        width: w,
        height: h,
        data: PixelData::Yuv8 { planes: [Arc::new(y), Arc::new(u), Arc::new(v)], chroma: Chroma::C420, alpha: None },
        color: filmcraft_color::ColorInfo::REC709,
        par: (1, 1),
        pts: Default::default(),
    })
}

#[test]
fn gpu_matches_cpu_plan() {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let mut c = GpuCompositor::new(&dev, &q);
    let pixels = filmcraft_media::generators::render(
        &filmcraft_media::Generator::Demo(filmcraft_media::DemoScene::OceanSunset),
        320,
        180,
        1.0,
        24,
        filmcraft_time::FrameRate::FPS_24,
    );
    let rgba = Arc::new(VideoFrame::rgba8(320, 180, pixels));
    let (w, h) = (320usize, 180usize);
    let plan = FramePlan::Layers {
        width: w,
        height: h,
        layers: vec![
            PlanLayer { frame: yuv_frame(640, 360), matrix: Affine::scale(0.5, 0.5), opacity: 1.0 },
            PlanLayer { frame: rgba, matrix: Affine::motion(Vec2::new(200.0, 100.0), Vec2::new(0.4, 0.4), 12.0, Vec2::new(160.0, 90.0)), opacity: 0.7 },
        ],
    };
    let cpu = execute_cpu(&plan).over_black_rgba8();
    c.composite(&plan);
    let (gw, gh, gpu) = c.read_output().expect("readback");
    assert_eq!((gw as usize, gh as usize), (w, h));
    // compare away from antialiased edges: mean abs error and 99th percentile
    let mut diffs: Vec<u32> =
        cpu.chunks(4).zip(gpu.chunks(4)).map(|(a, b)| (0..3).map(|k| (a[k] as i32 - b[k] as i32).unsigned_abs()).max().unwrap_or(0)).collect();
    diffs.sort_unstable();
    let p99 = diffs[diffs.len() * 99 / 100];
    let mean = diffs.iter().sum::<u32>() as f64 / diffs.len() as f64;
    assert!(p99 <= 6 && mean < 1.5, "p99 {p99}, mean {mean}");
    // cache: compositing the same plan again uploads nothing
    let before = c.uploaded_bytes;
    c.composite(&plan);
    assert_eq!(c.uploaded_bytes, before);
}

#[test]
fn half_float_conversion() {
    for v in [0.0f32, 1.0, 0.5, 0.123, 65504.0, -2.0, 1e-5] {
        let h = f32_to_f16(v);
        // decode
        let s = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
        let e = ((h >> 10) & 0x1f) as i32;
        let m = (h & 0x3ff) as f32;
        let d = if e == 0 { s * m * 2f32.powi(-24) } else { s * (1.0 + m / 1024.0) * 2f32.powi(e - 15) };
        assert!((d - v).abs() <= v.abs() * 1e-3 + 1e-6, "{v} → {d}");
    }
}

#[test]
fn prepared_upload_matches_inline_conversion() {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let (w, h) = (64u32, 36u32);
    let yuv16 = Arc::new(VideoFrame {
        width: w,
        height: h,
        data: PixelData::Yuv16 {
            planes: [
                Arc::new((0..w * h).map(|i| (64 + (i * 7) % 876) as u16).collect()),
                Arc::new((0..w * h / 2).map(|i| (64 + (i * 13) % 896) as u16).collect()),
                Arc::new((0..w * h / 2).map(|i| (960 - (i * 5) % 896) as u16).collect()),
            ],
            chroma: Chroma::C422,
            bits: 10,
            alpha: None,
        },
        color: filmcraft_color::ColorInfo::REC709,
        par: (1, 1),
        pts: Default::default(),
    });
    let f32_layer: Vec<f32> = (0..w * h * 4).map(|i| ((i * 37) % 1000) as f32 / 1000.0 * if i % 4 == 3 { 1.0 } else { 0.6 }).collect();
    let rgbaf = Arc::new(VideoFrame::rgba_f32(w, h, f32_layer.clone()));
    let layers = FramePlan::Layers {
        width: w as usize,
        height: h as usize,
        layers: vec![
            PlanLayer { frame: yuv16, matrix: Affine::IDENTITY, opacity: 1.0 },
            PlanLayer { frame: rgbaf, matrix: Affine::scale(0.5, 0.5), opacity: 0.8 },
        ],
    };
    let image = FramePlan::Image(filmcraft_render::Image { w: w as usize, h: h as usize, px: f32_layer });
    for plan in [layers, image] {
        let mut a = GpuCompositor::new(&dev, &q);
        a.composite(&plan);
        let inline = a.read_output().expect("readback");
        let mut b = GpuCompositor::new(&dev, &q);
        let prep = prepare(&plan);
        assert!(prep.bytes() > 0);
        b.composite_prepared(&plan, Some(&prep));
        let prepared = b.read_output().expect("readback");
        assert!(inline == prepared, "prepared upload differs");
    }
}
