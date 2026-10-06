//! The picture of nested sequences: a nest shows its sequence at that sequence's own size and
//! frame rate.
//! Premiere's behaviour was observed in Premiere Pro 26.5.2.

use super::*;
use filmcraft_media::generators::GeneratorSource;
use filmcraft_media::{DemoScene, Generator, MediaSource};
use filmcraft_project::{ClipId, Label, MediaClip, MediaRef, ParamValue, SequenceSettings, TrackKind};
use filmcraft_time::{FrameRate, TICKS_PER_SECOND, TimeRange};

const RED: [f32; 4] = [1.0, 0.0, 0.0, 1.0];

struct Rig {
    p: Project,
    map: SourceMap,
}

impl Rig {
    fn new() -> Rig {
        Rig { p: Project::new("nest"), map: SourceMap::default() }
    }

    fn add(&mut self, g: GeneratorSource) -> ItemId {
        let info = g.info().clone();
        let id = self.p.add_item(
            &info.name.clone(),
            Label::Iris,
            ItemKind::Media(MediaClip {
                media: MediaRef::Generator(g.generator.clone()),
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
        self.map.0.insert(id, Arc::new(g) as SharedSource);
        id
    }

    fn matte(&mut self, color: [f32; 4], w: u32, h: u32) -> ItemId {
        self.add(GeneratorSource::new(Generator::ColorMatte { color }, w, h, FrameRate::FPS_24, Tick(10 * TICKS_PER_SECOND)))
    }

    fn seq(&mut self, name: &str, w: u32, h: u32, rate: FrameRate) -> ItemId {
        self.p.new_sequence(name, SequenceSettings { width: w, height: h, frame_rate: rate, ..Default::default() }, 2, 1, None)
    }

    /// `frames` frames of `item` from its start, at the start of V1 of `seq`.
    fn put(&mut self, seq: ItemId, item: ItemId, frames: i64) -> ClipId {
        let rate = self.p.sequence(seq).unwrap().settings.frame_rate;
        let ti = self.p.make_track_item(item, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, rate.tick_of(frames)), rate).unwrap();
        let id = ti.id;
        self.p.sequence_mut(seq).unwrap().video_tracks[0].items.push(ti);
        id
    }

    fn clip(&mut self, seq: ItemId, clip: ClipId) -> &mut TrackItem {
        self.p.sequence_mut(seq).unwrap().find_item_mut(clip).unwrap().1
    }

    fn frame(&self, seq: ItemId, t: Tick) -> Image {
        render_sequence(&self.p, seq, t, RenderOptions::default(), &self.map)
    }

    /// The same frame through the frame plan (what the GPU compositor is given).
    fn planned(&self, seq: ItemId, t: Tick) -> Image {
        plan::execute_cpu(&plan::plan_frame(&self.p, seq, t, RenderOptions::default(), &self.map))
    }
}

fn close(a: [f32; 4], b: [f32; 4]) -> bool {
    a.iter().zip(&b).all(|(x, y)| (x - y).abs() < 0.02)
}

/// The largest channel difference between two images of the same size.
fn worst(a: &Image, b: &Image) -> f32 {
    assert_eq!((a.w, a.h), (b.w, b.h));
    a.px.iter().zip(&b.px).map(|(x, y)| (x - y).abs()).fold(0.0, f32::max)
}

#[test]
fn a_smaller_nest_is_shown_at_its_own_size_in_the_middle() {
    // Premiere: a 1280x720 sequence in a 1920x1080 one comes in centred at 100%
    let mut r = Rig::new();
    let red = r.matte(RED, 320, 180);
    let inner = r.seq("inner", 320, 180, FrameRate::FPS_24);
    r.put(inner, red, 48);
    let outer = r.seq("outer", 640, 360, FrameRate::FPS_24);
    let nest = r.put(outer, inner, 48);
    for img in [r.frame(outer, Tick(1000)), r.planned(outer, Tick(1000))] {
        assert_eq!((img.w, img.h), (640, 360));
        // the nest covers x 160..480, y 90..270
        assert!(close(img.get(320, 180), RED) && close(img.get(170, 100), RED) && close(img.get(470, 260), RED));
        assert!(img.get(150, 180)[3] < 0.02 && img.get(320, 80)[3] < 0.02 && img.get(10, 10)[3] < 0.02, "empty around it");
    }
    // Scale to Frame Size fills the frame without touching Motion ▸ Scale
    r.clip(outer, nest).scale_to_frame = true;
    let img = r.frame(outer, Tick(1000));
    assert!(close(img.get(10, 10), RED) && close(img.get(630, 350), RED));
    // Motion ▸ Scale 200% (what Fit to frame sets here) does the same
    let c = r.clip(outer, nest);
    c.scale_to_frame = false;
    c.effect_mut("motion").unwrap().params.get_mut("scale").unwrap().value = ParamValue::Float(200.0);
    let img = r.frame(outer, Tick(1000));
    assert!(close(img.get(10, 10), RED) && close(img.get(630, 350), RED));
}

#[test]
fn a_larger_nest_is_cropped_by_the_frame() {
    let mut r = Rig::new();
    let ocean = r.add(GeneratorSource::demo(DemoScene::OceanSunset));
    let (w, h) = {
        let v = r.map.0[&ocean].info().video.clone().unwrap();
        (v.width, v.height)
    };
    let inner = r.seq("inner", w, h, FrameRate::FPS_24);
    r.put(inner, ocean, 48);
    let outer = r.seq("outer", w / 2, h / 2, FrameRate::FPS_24);
    r.put(outer, inner, 48);
    let (big, small) = (r.frame(inner, Tick(1000)), r.frame(outer, Tick(1000)));
    assert_eq!((small.w as u32, small.h as u32), (w / 2, h / 2));
    // the outer frame is the middle of the inner one, pixel for pixel
    let (ox, oy) = (big.w / 4, big.h / 4);
    for (x, y) in [(0, 0), (small.w / 2, small.h / 2), (small.w - 1, small.h - 1), (7, small.h - 3)] {
        assert!(close(small.get(x, y), big.get(x + ox, y + oy)), "({x}, {y})");
    }
}

#[test]
fn a_nest_at_another_frame_rate_shows_its_sequence_at_the_same_moment() {
    let mut r = Rig::new();
    let ocean = r.add(GeneratorSource::demo(DemoScene::OceanSunset));
    let (w, h) = {
        let v = r.map.0[&ocean].info().video.clone().unwrap();
        (v.width, v.height)
    };
    let inner = r.seq("inner", w, h, FrameRate::FPS_30);
    r.put(inner, ocean, 90);
    let outer = r.seq("outer", w, h, FrameRate::FPS_24);
    r.put(outer, inner, 72);
    // 1.5 s into the 24 fps sequence is 1.5 s into the 30 fps one
    let t = FrameRate::FPS_24.tick_of(36);
    assert!(worst(&r.frame(outer, t), &r.frame(inner, t)) < 0.01);
    assert!(worst(&r.frame(inner, t), &r.frame(inner, Tick::ZERO)) > 0.05, "the footage moves, so the comparison means something");
}
