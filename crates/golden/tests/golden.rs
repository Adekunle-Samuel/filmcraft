//! Golden-image tests: procedural projects rendered through the CPU compositor and compared with
//! committed reference PNGs (`goldens/*.png`, made by this test with `FILMCRAFT_BLESS=1`), plus
//! GPU-vs-CPU parity on the same scenes when a GPU adapter is available.
//!
//! Tolerances (8-bit sRGB levels, see `filmcraft_testkit::golden`):
//! - CPU vs golden: `Tolerance::RENDER` — PSNR ≥ 45 dB, max abs ≤ 12, 99th percentile ≤ 2.
//! - GPU vs CPU: 99th percentile of the per-pixel max channel difference ≤ 6 and mean < 1.5
//!   (antialiased edges and half-float textures differ slightly; the CPU is the reference).

use std::path::PathBuf;
use std::sync::Arc;

use filmcraft_geom::Vec2;
use filmcraft_media::generators::GeneratorSource;
use filmcraft_media::{DemoScene, Generator, MediaSource, SharedSource};
use filmcraft_project::{
    ClipId, EffectInstance, ItemId, ItemKind, Label, MediaClip, MediaRef, ParamValue, Project, SequenceSettings, TrackItem, TrackKind, Transition,
    TransitionId, find_effect,
};
use filmcraft_render::{RenderOptions, SourceMap, render_sequence};
use filmcraft_testkit::golden::{Rgba8, Tolerance, assert_golden, diff};
use filmcraft_time::{FrameRate, TICKS_PER_SECOND, Tick, TimeRange};

const W: u32 = 320;
const H: u32 = 180;
const RATE: FrameRate = FrameRate::FPS_24;

/// A procedurally generated project and the frame to render.
struct Scene {
    project: Project,
    seq: ItemId,
    sources: SourceMap,
    frame: i64,
}

struct Builder {
    p: Project,
    map: SourceMap,
    seq: ItemId,
    next_transition: u64,
}

impl Builder {
    fn new(video_tracks: usize) -> Self {
        let mut p = Project::new("golden");
        let seq = p.new_sequence("golden", SequenceSettings { width: W, height: H, frame_rate: RATE, ..Default::default() }, video_tracks, 0, None);
        Builder { p, map: SourceMap::default(), seq, next_transition: 1 }
    }

    /// A generated media item (640×360, 24 fps, 10 s).
    fn media(&mut self, g: Generator) -> ItemId {
        let src = GeneratorSource::new(g, 640, 360, RATE, Tick(10 * TICKS_PER_SECOND));
        let info = src.info().clone();
        let id = self.p.add_item(
            &info.name.clone(),
            Label::Iris,
            ItemKind::Media(MediaClip {
                media: MediaRef::Generator(src.generator.clone()),
                info,
                interpret: Default::default(),
                mark_in: None,
                mark_out: None,
                markers: vec![],
                offline: false,
                proxy: None,
                identity: None,
            }),
            None,
        );
        self.map.0.insert(id, Arc::new(src) as SharedSource);
        id
    }

    fn demo(&mut self, scene: DemoScene) -> ItemId {
        self.media(Generator::Demo(scene))
    }

    /// Place `item` on video track `track` (frames), scaled to the frame size.
    fn place(&mut self, track: usize, item: ItemId, start: i64, dur: i64) -> ClipId {
        let mut ti = self.p.make_track_item(item, TrackKind::Video, RATE.tick_of(start), TimeRange::new(Tick::ZERO, RATE.tick_of(dur)), RATE).unwrap();
        ti.scale_to_frame = true;
        let id = ti.id;
        let s = self.p.sequence_mut(self.seq).unwrap();
        s.video_tracks[track].items.push(ti);
        s.video_tracks[track].sort();
        id
    }

    fn clip(&mut self, id: ClipId) -> &mut TrackItem {
        self.p.sequence_mut(self.seq).unwrap().find_item_mut(id).unwrap().1
    }

    /// Set intrinsic-effect parameters (`motion`, `opacity`).
    fn fixed(&mut self, id: ClipId, effect: &str, params: &[(&str, ParamValue)]) {
        let e = self.clip(id).effect_mut(effect).unwrap();
        set(e, params);
    }

    /// Append a standard effect with parameters.
    fn effect(&mut self, id: ClipId, effect: &str, params: &[(&str, ParamValue)]) {
        let mut e = find_effect(effect).unwrap_or_else(|| panic!("no effect {effect}")).instance();
        set(&mut e, params);
        self.clip(id).effects.push(e);
    }

    /// A transition of `dur` frames centred on the cut at `cut` between `a` and `b` on `track`.
    fn transition(&mut self, track: usize, effect: &str, a: ClipId, b: ClipId, cut: i64, dur: i64) {
        let id = TransitionId(self.next_transition);
        self.next_transition += 1;
        let tr = Transition {
            id,
            effect: find_effect(effect).unwrap_or_else(|| panic!("no transition {effect}")).instance(),
            start: RATE.tick_of(cut - dur / 2),
            duration: RATE.tick_of(dur),
            from: Some(a),
            to: Some(b),
            align: Default::default(),
            reverse: false,
        };
        self.p.sequence_mut(self.seq).unwrap().video_tracks[track].transitions.push(tr);
    }

    fn at(self, frame: i64) -> Scene {
        Scene { project: self.p, seq: self.seq, sources: self.map, frame }
    }
}

fn set(e: &mut EffectInstance, params: &[(&str, ParamValue)]) {
    let id = e.effect.clone();
    for (k, v) in params {
        e.param_mut(k).unwrap_or_else(|| panic!("{id}: no param {k}")).value = v.clone();
    }
}

fn fl(v: f64) -> ParamValue {
    ParamValue::Float(v)
}

fn pt(x: f64, y: f64) -> ParamValue {
    ParamValue::Vec2(Vec2::new(x, y))
}

fn blend(name: &str) -> ParamValue {
    ParamValue::Choice(filmcraft_project::effect::BLEND_MODES.iter().position(|b| *b == name).unwrap_or_else(|| panic!("blend {name}")) as u32)
}

// ---- scenes ----

/// Motion (scale, rotation, position) and 70 % opacity over a full-frame background.
fn transform_opacity() -> Scene {
    let mut b = Builder::new(2);
    let bg = b.demo(DemoScene::OceanSunset);
    let fg = b.demo(DemoScene::Aurora);
    b.place(0, bg, 0, 48);
    let top = b.place(1, fg, 0, 48);
    b.fixed(top, "motion", &[("scale", fl(45.0)), ("rotation", fl(20.0)), ("position", pt(215.0, 70.0))]);
    b.fixed(top, "opacity", &[("opacity", fl(70.0))]);
    b.at(12)
}

/// Four quadrant layers over a background in Multiply, Screen, Overlay and Difference.
fn blend_modes() -> Scene {
    let mut b = Builder::new(5);
    let bg = b.demo(DemoScene::OceanSunset);
    let fg = b.demo(DemoScene::Dunes);
    b.place(0, bg, 0, 48);
    for (i, (mode, x, y)) in [("Multiply", 80.0, 45.0), ("Screen", 240.0, 45.0), ("Overlay", 80.0, 135.0), ("Difference", 240.0, 135.0)].into_iter().enumerate()
    {
        let c = b.place(i + 1, fg, 0, 48);
        b.fixed(c, "motion", &[("scale", fl(50.0)), ("position", pt(x, y))]);
        b.fixed(c, "opacity", &[("blend", blend(mode))]);
    }
    b.at(12)
}

/// Gaussian Blur on colour bars (sharp edges show the kernel).
fn gaussian_blur() -> Scene {
    let mut b = Builder::new(1);
    let bars = b.media(Generator::BarsAndTone);
    let c = b.place(0, bars, 0, 48);
    b.effect(c, "gaussian_blur", &[("blurriness", fl(12.0))]);
    b.at(12)
}

/// Lumetri Color basic correction: temperature, exposure, contrast, highlights/shadows, saturation.
fn lumetri_basic() -> Scene {
    let mut b = Builder::new(1);
    let bg = b.demo(DemoScene::OceanSunset);
    let c = b.place(0, bg, 0, 48);
    b.effect(
        c,
        "lumetri",
        &[
            ("temperature", fl(25.0)),
            ("exposure", fl(0.6)),
            ("contrast", fl(30.0)),
            ("highlights", fl(-30.0)),
            ("shadows", fl(25.0)),
            ("saturation", fl(135.0)),
        ],
    );
    b.at(12)
}

/// Crop (left/top/right/bottom) of a top layer revealing the background.
fn crop() -> Scene {
    let mut b = Builder::new(2);
    let bg = b.demo(DemoScene::OceanSunset);
    let bars = b.media(Generator::BarsAndTone);
    b.place(0, bg, 0, 48);
    let c = b.place(1, bars, 0, 48);
    b.effect(c, "crop", &[("left", fl(15.0)), ("top", fl(10.0)), ("right", fl(25.0)), ("bottom", fl(20.0))]);
    b.at(12)
}

/// Two clips with a 12-frame transition centred on the cut at frame 24, rendered at `frame`.
fn transition(effect: &str, frame: i64) -> Scene {
    let mut b = Builder::new(1);
    let a = b.demo(DemoScene::OceanSunset);
    let c = b.demo(DemoScene::Aurora);
    let ca = b.place(0, a, 0, 24);
    let cb = b.place(0, c, 24, 24);
    b.transition(0, effect, ca, cb, 24, 12);
    b.at(frame)
}

/// Timecode and Clip Name burn-ins (text engine: JetBrains Mono / Inter).
fn text_burnin() -> Scene {
    let mut b = Builder::new(1);
    let bg = b.demo(DemoScene::CityNight);
    let c = b.place(0, bg, 0, 48);
    b.effect(c, "timecode", &[("position", pt(160.0, 40.0)), ("size", fl(20.0))]);
    b.effect(c, "clip_name", &[("position", pt(160.0, 145.0)), ("size", fl(12.0))]);
    b.at(30)
}

/// A graphic clip on V2 over `bg` on V1, with `layers` (graphic layer effect instances).
fn graphic_scene(bg: DemoScene, layers: Vec<EffectInstance>) -> Scene {
    let mut b = Builder::new(2);
    let bgi = b.demo(bg);
    b.place(0, bgi, 0, 48);
    let g = b.p.add_item("Graphic", Label::Rose, ItemKind::Graphic { width: W, height: H, rate: RATE }, None);
    let c = b.place(1, g, 0, 48);
    for mut l in layers {
        filmcraft_project::resolve_auto_points(&mut l, (W, H), (W, H));
        b.clip(c).effects.push(l);
    }
    b.at(12)
}

/// A title: bold centred text with an outer stroke and a soft drop shadow, a lower-third bar with
/// rounded corners and a background-boxed caption line.
fn graphic_title() -> Scene {
    use filmcraft_project::graphic::{new_shape_layer, new_text_layer};
    let mut bar = new_shape_layer(0, Vec2::new(160.0, 150.0), Vec2::new(280.0, 34.0), vec![]);
    set(&mut bar, &[("corner_radius", fl(8.0)), ("fill_color", ParamValue::Color([0.12, 0.35, 0.8, 1.0])), ("opacity", fl(85.0))]);
    let mut title = new_text_layer("Night Drive", Vec2::new(160.0, 80.0), 44.0);
    set(
        &mut title,
        &[
            ("font_style", ParamValue::Text("Bold".into())),
            ("align", ParamValue::Choice(1)),
            ("stroke", ParamValue::Bool(true)),
            ("stroke_width", fl(2.5)),
            ("stroke_color", ParamValue::Color([0.05, 0.05, 0.1, 1.0])),
            ("shadow", ParamValue::Bool(true)),
            ("shadow_distance", fl(5.0)),
            ("shadow_blur", fl(8.0)),
            ("tracking", fl(40.0)),
        ],
    );
    let mut sub = new_text_layer("Directed by Nobody", Vec2::new(160.0, 156.0), 16.0);
    set(
        &mut sub,
        &[
            ("align", ParamValue::Choice(1)),
            ("font", ParamValue::Text("Noto Serif".into())),
            ("faux_italic", ParamValue::Bool(true)),
            ("caps", ParamValue::Choice(2)),
            ("fill_color", ParamValue::Color([1.0, 0.92, 0.6, 1.0])),
        ],
    );
    graphic_scene(DemoScene::CityNight, vec![bar, title, sub])
}

/// Shapes: ellipse with centre stroke, rotated polygon with inner stroke, a pen path, rotated
/// boxed text with a background and 2 strokes.
fn graphic_shapes() -> Scene {
    use filmcraft_project::graphic::{new_shape_layer, new_text_layer};
    let mut ell = new_shape_layer(1, Vec2::new(70.0, 60.0), Vec2::new(100.0, 70.0), vec![]);
    set(
        &mut ell,
        &[
            ("stroke", ParamValue::Bool(true)),
            ("stroke_width", fl(6.0)),
            ("stroke_type", ParamValue::Choice(1)),
            ("stroke_color", ParamValue::Color([1.0, 1.0, 1.0, 1.0])),
        ],
    );
    let mut poly = new_shape_layer(2, Vec2::new(250.0, 60.0), Vec2::new(80.0, 80.0), vec![]);
    set(
        &mut poly,
        &[
            ("sides", fl(5.0)),
            ("rotation", fl(18.0)),
            ("fill_color", ParamValue::Color([0.2, 0.8, 0.4, 1.0])),
            ("stroke", ParamValue::Bool(true)),
            ("stroke_type", ParamValue::Choice(2)),
            ("stroke_width", fl(5.0)),
        ],
    );
    let path = new_shape_layer(3, Vec2::new(70.0, 140.0), Vec2::new(0.0, 0.0), vec![[-40.0, 20.0], [0.0, -25.0], [40.0, 20.0], [0.0, 5.0]]);
    let mut txt = new_text_layer("Rotated\nTwo lines", Vec2::new(215.0, 140.0), 20.0);
    set(
        &mut txt,
        &[
            ("rotation", fl(-12.0)),
            ("align", ParamValue::Choice(1)),
            ("background", ParamValue::Bool(true)),
            ("background_radius", fl(6.0)),
            ("background_size", fl(6.0)),
            ("stroke", ParamValue::Bool(true)),
            ("stroke_width", fl(1.5)),
            ("stroke2", ParamValue::Bool(true)),
            ("stroke2_width", fl(3.5)),
        ],
    );
    graphic_scene(DemoScene::Aurora, vec![ell, poly, path, txt])
}

/// (name, title, scene).
fn scenes() -> Vec<(&'static str, &'static str, fn() -> Scene)> {
    vec![
        ("transform_opacity", "Motion transform and opacity", transform_opacity),
        ("blend_modes", "Blend modes (Multiply, Screen, Overlay, Difference)", blend_modes),
        ("gaussian_blur", "Gaussian Blur effect", gaussian_blur),
        ("lumetri_basic", "Lumetri Color basic correction", lumetri_basic),
        ("crop", "Crop effect", crop),
        ("transition_cross_dissolve", "Cross Dissolve at 50%", || transition("cross_dissolve", 24)),
        ("transition_dip_to_black", "Dip to Black at 25%", || transition("dip_to_black", 21)),
        ("transition_wipe", "Wipe at 50%", || transition("wipe", 24)),
        ("text_burnin", "Timecode and Clip Name burn-in text", text_burnin),
        ("graphic_title", "Graphic clip: title text with stroke and shadow, lower-third bar", graphic_title),
        ("graphic_shapes", "Graphic clip: ellipse, polygon, path and rotated text with background", graphic_shapes),
    ]
}

fn render_cpu(s: &Scene) -> Rgba8 {
    let img = render_sequence(&s.project, s.seq, RATE.tick_of(s.frame), RenderOptions::default(), &s.sources);
    Rgba8::new(img.w as u32, img.h as u32, img.over_black_rgba8())
}

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("goldens").join(format!("{name}.png"))
}

fn check(name: &str) {
    let (_, title, make) = scenes().into_iter().find(|(n, _, _)| *n == name).unwrap();
    let img = render_cpu(&make());
    assert_eq!((img.w, img.h), (W, H));
    let d = assert_golden(&golden_path(name), &img, Tolerance::RENDER, title, &format!("crates/golden/tests/golden.rs ({name})"));
    eprintln!("{name}: {d}");
}

macro_rules! goldens {
    ($($name:ident),* $(,)?) => {
        mod cpu {
            $(
                #[test]
                fn $name() {
                    super::check(stringify!($name));
                }
            )*
        }

        #[test]
        fn every_scene_has_a_test() {
            let tested = [$(stringify!($name)),*];
            for (n, _, _) in scenes() {
                assert!(tested.contains(&n), "scene {n} has no golden test");
            }
        }
    };
}

goldens!(
    transform_opacity,
    blend_modes,
    gaussian_blur,
    lumetri_basic,
    crop,
    transition_cross_dissolve,
    transition_dip_to_black,
    transition_wipe,
    text_burnin,
    graphic_title,
    graphic_shapes
);

/// Sanity: the scenes are not trivially empty or identical to each other.
#[test]
fn scenes_are_distinct() {
    let imgs: Vec<(&str, Rgba8)> = scenes().into_iter().map(|(n, _, f)| (n, render_cpu(&f()))).collect();
    for (n, img) in &imgs {
        let lit = img.px.chunks_exact(4).filter(|p| p[0] as u32 + p[1] as u32 + p[2] as u32 > 30).count();
        assert!(lit > img.px.len() / 4 / 4, "{n}: mostly black");
    }
    for i in 0..imgs.len() {
        for j in i + 1..imgs.len() {
            let d = diff(&imgs[i].1, &imgs[j].1).unwrap();
            assert!(d.psnr < 30.0, "{} and {} are near-identical ({d})", imgs[i].0, imgs[j].0);
        }
    }
}

// ---- GPU parity ----

fn device() -> Option<(wgpu::Device, wgpu::Queue)> {
    let instance = wgpu::Instance::default();
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())).ok()?;
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()
}

#[test]
fn gpu_matches_cpu_on_golden_scenes() {
    let Some((dev, q)) = device() else {
        eprintln!("SKIPPED (gpu parity): no GPU adapter");
        return;
    };
    let mut c = filmcraft_gpu::GpuCompositor::new(&dev, &q);
    let mut failures = Vec::new();
    for (name, _, make) in scenes() {
        let s = make();
        let t = RATE.tick_of(s.frame);
        let cpu = render_cpu(&s);
        let plan = filmcraft_render::plan::plan_frame(&s.project, s.seq, t, RenderOptions::default(), &s.sources);
        let kind = if matches!(plan, filmcraft_render::plan::FramePlan::Layers { .. }) { "layers" } else { "cpu image" };
        c.composite(&plan);
        let (gw, gh, mut px) = c.read_output().expect("GPU readback");
        px.chunks_exact_mut(4).for_each(|p| p[3] = 255);
        let gpu = Rgba8::new(gw, gh, px);
        let d = diff(&cpu, &gpu).unwrap();
        // mean over RGB only (alpha is equal by construction)
        let mean_rgb = d.mean_abs * 4.0 / 3.0;
        eprintln!("{name} ({kind}): GPU vs CPU {d}");
        if d.p99 > 6 || mean_rgb >= 1.5 {
            let dir = filmcraft_testkit::golden::failures_dir();
            let _ = filmcraft_testkit::golden::write_png(&dir.join(format!("{name}.gpu.png")), &gpu);
            let _ = filmcraft_testkit::golden::write_png(&dir.join(format!("{name}.cpu.png")), &cpu);
            failures.push(format!("{name} ({kind}): {d}; images in {}", dir.display()));
        }
    }
    assert!(failures.is_empty(), "GPU differs from the CPU reference:\n{}", failures.join("\n"));
}
