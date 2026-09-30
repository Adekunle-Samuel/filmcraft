use super::*;
use filmcraft_media::generators::GeneratorSource;
use filmcraft_media::{DemoScene, Generator, MediaSource};
use filmcraft_project::{Label, MediaClip, MediaRef, SequenceSettings, TrackKind};
use filmcraft_time::{FrameRate, TICKS_PER_SECOND, TimeRange};

fn setup() -> (Project, ItemId, ItemId, ItemId, SourceMap) {
    let mut p = Project::new("r");
    let mut map = SourceMap::default();
    let mut add = |p: &mut Project, g: GeneratorSource| {
        let info = g.info().clone();
        let generator = g.generator.clone();
        let id = p.add_item(
            &info.name.clone(),
            Label::Iris,
            ItemKind::Media(MediaClip {
                media: MediaRef::Generator(generator),
                info,
                interpret: Default::default(),
                mark_in: None,
                mark_out: None,
                markers: vec![],
                offline: false,
                proxy: None,
            }),
            None,
        );
        map.0.insert(id, Arc::new(g) as SharedSource);
        id
    };
    let red =
        add(&mut p, GeneratorSource::new(Generator::ColorMatte { color: [1.0, 0.0, 0.0, 1.0] }, 320, 180, FrameRate::FPS_24, Tick(10 * TICKS_PER_SECOND)));
    let ocean = add(&mut p, GeneratorSource::demo(DemoScene::OceanSunset));
    let seq = p.new_sequence("s", SequenceSettings { width: 320, height: 180, frame_rate: FrameRate::FPS_24, ..Default::default() }, 2, 2, None);
    (p, red, ocean, seq, map)
}

fn place(p: &mut Project, seq: ItemId, track: usize, item: ItemId, start_f: i64, dur_f: i64) -> filmcraft_project::ClipId {
    let r = FrameRate::FPS_24;
    let ti = p.make_track_item(item, TrackKind::Video, r.tick_of(start_f), TimeRange::new(Tick::ZERO, r.tick_of(dur_f)), r).unwrap();
    let id = ti.id;
    let mut ti = ti;
    ti.scale_to_frame = true;
    p.sequence_mut(seq).unwrap().video_tracks[track].items.push(ti);
    p.sequence_mut(seq).unwrap().video_tracks[track].sort();
    id
}

#[test]
fn composites_tracks_and_opacity() {
    let (mut p, red, ocean, seq, map) = setup();
    place(&mut p, seq, 0, ocean, 0, 48);
    let top = place(&mut p, seq, 1, red, 0, 48);
    let img = render_sequence(&p, seq, Tick(1000), RenderOptions::default(), &map);
    assert_eq!((img.w, img.h), (320, 180));
    let c = img.get(160, 90);
    assert!((c[0] - 1.0).abs() < 1e-4 && c[1] < 1e-4, "red on top: {c:?}");
    // 50% opacity shows the bottom layer through
    let s = p.sequence_mut(seq).unwrap();
    let (_, it) = s.find_item_mut(top).unwrap();
    it.effect_mut("opacity").unwrap().params.get_mut("opacity").unwrap().value = ParamValue::Float(50.0);
    let img = render_sequence(&p, seq, Tick(1000), RenderOptions::default(), &map);
    let c = img.get(160, 20);
    assert!(c[0] > 0.5 && c[0] < 1.0 && c[2] > 0.0, "{c:?}");
}

#[test]
fn half_resolution_matches_full_downsampled() {
    let (mut p, _red, ocean, seq, map) = setup();
    place(&mut p, seq, 0, ocean, 0, 48);
    let full = render_sequence(&p, seq, Tick(1000), RenderOptions::default(), &map);
    let half = render_sequence(&p, seq, Tick(1000), RenderOptions { scale: 0.5, ..Default::default() }, &map);
    assert_eq!((half.w, half.h), (160, 90));
    let a = full.get(200, 60);
    let b = half.get(100, 30);
    for k in 0..3 {
        assert!((a[k] - b[k]).abs() < 0.06, "{a:?} vs {b:?}");
    }
}

#[test]
fn motion_scale_and_position() {
    let (mut p, red, _o, seq, map) = setup();
    let id = place(&mut p, seq, 0, red, 0, 48);
    let s = p.sequence_mut(seq).unwrap();
    let (_, it) = s.find_item_mut(id).unwrap();
    let m = it.effect_mut("motion").unwrap();
    m.params.get_mut("scale").unwrap().value = ParamValue::Float(50.0);
    m.params.get_mut("position").unwrap().value = ParamValue::Vec2(filmcraft_geom::Vec2::new(80.0, 45.0));
    let img = render_sequence(&p, seq, Tick(0), RenderOptions::default(), &map);
    assert!(img.get(80, 45)[3] > 0.99);
    assert!(img.get(10, 10)[3] > 0.99, "top-left quadrant covered");
    assert_eq!(img.get(200, 120)[3], 0.0, "rest transparent");
}

#[test]
fn cross_dissolve_midpoint() {
    let (mut p, red, _o, seq, map) = setup();
    let a = place(&mut p, seq, 0, red, 0, 24);
    let blue_src = GeneratorSource::new(Generator::ColorMatte { color: [0.0, 0.0, 1.0, 1.0] }, 320, 180, FrameRate::FPS_24, Tick(10 * TICKS_PER_SECOND));
    let info = blue_src.info().clone();
    let blue = p.add_item(
        "blue",
        Label::Iris,
        ItemKind::Media(MediaClip {
            media: MediaRef::Generator(blue_src.generator.clone()),
            info,
            interpret: Default::default(),
            mark_in: None,
            mark_out: None,
            markers: vec![],
            offline: false,
            proxy: None,
        }),
        None,
    );
    let mut map = map;
    map.0.insert(blue, Arc::new(blue_src));
    let b = place(&mut p, seq, 0, blue, 24, 24);
    let r = FrameRate::FPS_24;
    let tr = filmcraft_project::Transition {
        id: filmcraft_project::TransitionId(999),
        effect: filmcraft_project::find_effect("cross_dissolve").unwrap().instance(),
        start: r.tick_of(18),
        duration: r.tick_of(12),
        from: Some(a),
        to: Some(b),
        align: Default::default(),
        reverse: false,
    };
    p.sequence_mut(seq).unwrap().video_tracks[0].transitions.push(tr);
    let img = render_sequence(&p, seq, r.tick_of(24), RenderOptions::default(), &map);
    let c = img.get(10, 10);
    assert!((c[0] - 0.5).abs() < 0.05 && (c[2] - 0.5).abs() < 0.05, "{c:?}");
}

#[test]
fn audio_mix_bars_tone() {
    let mut p = Project::new("a");
    let g = GeneratorSource::new(Generator::BarsAndTone, 64, 36, FrameRate::FPS_24, Tick(5 * TICKS_PER_SECOND));
    let info = g.info().clone();
    let id = p.add_item(
        "bars",
        Label::Iris,
        ItemKind::Media(MediaClip {
            media: MediaRef::Generator(Generator::BarsAndTone),
            info,
            interpret: Default::default(),
            mark_in: None,
            mark_out: None,
            markers: vec![],
            offline: false,
            proxy: None,
        }),
        None,
    );
    let mut map = SourceMap::default();
    map.0.insert(id, Arc::new(g));
    let seq = p.new_sequence("s", SequenceSettings::default(), 1, 1, None);
    let r = FrameRate::FPS_23_976;
    let mut ti = p.make_track_item(id, TrackKind::Audio, Tick::ZERO, TimeRange::new(Tick::ZERO, Tick(TICKS_PER_SECOND)), r).unwrap();
    ti.effect_mut("volume").unwrap().params.get_mut("level").unwrap().value = ParamValue::Float(-6.0);
    p.sequence_mut(seq).unwrap().audio_tracks[0].items.push(ti);
    let s = p.sequence(seq).unwrap();
    let buf = audio::mix_sequence(&p, s, 0, 4800, &map);
    let peak = buf.peaks()[0];
    let expect = 10f32.powf(-26.0 / 20.0);
    assert!((peak - expect).abs() < 0.01, "{peak} vs {expect}");
}
