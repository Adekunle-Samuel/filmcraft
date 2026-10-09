//! Edit plan parsing, validation and compiling, plus a property test: hostile plans never panic
//! and never put a cut boundary inside a kept word.

use super::*;
use filmcraft_project::{ClipId, ItemId, Label, Project, SequenceSettings, TrackItem};
use proptest::prelude::*;

const R: FrameRate = FrameRate { num: 25, den: 1 };

fn s(x: f64) -> Tick {
    Tick((x * TICKS_PER_SECOND as f64).round() as i64)
}

/// A sequence `dur` seconds long (one audio clip).
fn seq(dur: f64, rate: FrameRate) -> Sequence {
    let mut p = Project::new("t");
    let id = p.new_sequence("s", SequenceSettings { frame_rate: rate, ..Default::default() }, 1, 1, None);
    let mut q = p.sequence(id).unwrap().clone();
    q.audio_tracks[0].items.push(TrackItem {
        id: ClipId(100),
        item: ItemId(1),
        name: "talk".into(),
        label: Label::Iris,
        start: Tick::ZERO,
        duration: s(dur),
        source_in: Tick::ZERO,
        speed: 1.0,
        reverse: false,
        enabled: true,
        link: None,
        group: None,
        effects: vec![],
        markers: vec![],
        gain_db: 0.0,
        frame_hold: None,
        scale_to_frame: false,
        essential: None,
        multicam: None,
        time_interpolation: Default::default(),
        hold_filters: false,
        field_options: None,
        source_channels: Vec::new(),
        graphic: None,
    });
    q
}

fn word(i: usize, text: &str, a: Tick, b: Tick) -> SeqWord {
    SeqWord { text: text.into(), start: a, end: b, clip: ClipId(100), item: ItemId(1), index: i, track: 0, speaker: None, confidence: 1.0 }
}

/// "Hello um world." then a 1.2 s pause, then "Second speaker here." in a 5 s sequence.
fn words() -> Vec<SeqWord> {
    [("Hello", 0.20, 0.50), ("um", 0.60, 0.90), ("world.", 1.00, 1.40), ("Second", 2.60, 3.00), ("speaker", 3.00, 3.50), ("here.", 3.50, 4.00)]
        .iter()
        .enumerate()
        .map(|(i, (t, a, b))| word(i, t, s(*a), s(*b)))
        .collect()
}

fn plan(json: &str) -> EditPlan {
    parse_plan(json).unwrap()
}

fn run(json: &str) -> CompiledPlan {
    compile(&plan(json), &seq(5.0, R), &words(), R).unwrap()
}

fn err(json: &str) -> String {
    compile(&plan(json), &seq(5.0, R), &words(), R).unwrap_err().to_string()
}

/// No removal boundary strictly inside a word that is not wholly removed.
fn assert_no_clipped_words(c: &CompiledPlan, words: &[SeqWord], dur: Tick) {
    let mut last = Tick::MIN;
    for r in &c.removals {
        assert!(r.range.duration > Tick::ZERO, "{r:?}");
        assert!(r.range.start > last || last == Tick::MIN, "sorted and not touching: {:?}", c.removals);
        assert!(r.range.start >= Tick::ZERO && r.range.end() <= dur, "{r:?} outside 0..{dur:?}");
        last = r.range.end();
    }
    let covered = |w: &SeqWord| c.removals.iter().any(|r| r.range.start <= w.start && r.range.end() >= w.end);
    for w in words.iter().filter(|w| w.end > w.start && !covered(w)) {
        for r in &c.removals {
            for b in [r.range.start, r.range.end()] {
                assert!(!(w.start < b && b < w.end), "boundary {b:?} inside kept word {w:?}; removals {:?}", c.removals);
            }
        }
    }
    let total = c.removals.iter().fold(Tick::ZERO, |a, r| a + r.range.duration);
    assert_eq!(c.after, c.before - total);
}

#[test]
fn a_minimal_plan_parses_and_changes_nothing() {
    let c = run(r#"{"version":1,"title":"x"}"#);
    assert!(c.removals.is_empty());
    assert_eq!(c.before, s(5.0));
    assert_eq!(c.after, c.before);
    assert_eq!(c.segments, 1);
    assert!(c.warnings.is_empty(), "{:?}", c.warnings);
}

#[test]
fn a_full_plan_round_trips() {
    let text = r#"{"version":1,"title":"Tight cut","rationale":"why","source":{"sequence":7},
        "output":{"mode":"inPlace","name":"Cut","aspect":"9:16"},
        "cuts":{"removeWords":[{"from":1,"reason":"filler"}],"removeRanges":[{"startS":1.5,"endS":2.0,"reason":"dead air"}],"keepOnly":[{"from":0,"to":5}]},
        "cleanup":{"fillers":[],"pauses":{"minS":1.0,"keepS":0.1},"silences":[{"startS":1.4,"endS":2.6}],"untranscribed":[]},
        "captions":{"maxChars":32,"lines":1,"burnIn":true,"style":{"font":"x"},"template":"t"},
        "grade":{"matchItem":3,"lut":"a.cube","lutStrength":0.5,"preset":"warm"},
        "audio":{"targetLufs":-16.0},
        "markers":[{"timeS":1.0,"name":"intro"},{"word":3,"name":"second"}],
        "targetDurationS":3.0,"export":{"preset":"H.264","path":"/tmp/x.mp4"}}"#;
    let p = plan(text);
    assert_eq!(p.output.mode, OutputMode::InPlace);
    assert_eq!(p.output.aspect, Some(Aspect::Vertical));
    let again = parse_plan(&serde_json::to_string(&p).unwrap()).unwrap();
    assert_eq!(again, p);
    assert!(validate(&p, 6).is_empty(), "{:?}", validate(&p, 6));
    // PascalCase mode names are accepted too
    assert_eq!(plan(r#"{"version":1,"title":"x","output":{"mode":"NewSequence"}}"#).output.mode, OutputMode::NewSequence);
}

#[test]
fn hostile_json_is_refused() {
    for bad in [
        r#"{"version":1,"title":"x","extra":1}"#,
        r#"{"version":1,"title":"x","cuts":{"removeWords":[{"from":0,"reason":"r","why":1}]}}"#,
        r#"{"version":1,"title":"x","output":{"aspect":"2:1"}}"#,
        r#"{"version":1,"title":"x","cuts":{"removeWords":[{"from":-1,"reason":"r"}]}}"#,
        r#"{"version":1,"title":"x","cuts":{"removeWords":[{"from":1e30,"reason":"r"}]}}"#,
        r#"{"version":1,"title":"x","cuts":{"removeRanges":[{"startS":1e999,"endS":2,"reason":"r"}]}}"#,
        r#"{"version":1}"#,
        r#"{"title":"x"}"#,
        r#"[]"#,
        "",
    ] {
        assert!(matches!(parse_plan(bad), Err(PlanError::Parse(_))), "{bad}");
    }
    let big = format!(r#"{{"version":1,"title":"x","rationale":"{}"}}"#, "a".repeat(3 << 20));
    assert_eq!(parse_plan(&big), Err(PlanError::TooLarge(big.len())));
    // deep nesting in `captions.style` hits serde_json's recursion limit, not the stack
    let deep = format!(r#"{{"version":1,"title":"x","captions":{{"style":{}{}}}}}"#, "[".repeat(10_000), "]".repeat(10_000));
    assert!(parse_plan(&deep).is_err());
}

#[test]
fn bad_values_are_named() {
    let e = err(r#"{"version":2,"title":""}"#);
    assert!(e.contains("version: 2") && e.contains("title: is empty"), "{e}");
    let e = err(r#"{"version":1,"title":"x","cuts":{"removeWords":[{"from":2,"to":99,"reason":"r"}]}}"#);
    assert!(e.contains("cuts.removeWords[0].to: word 99 is out of range (the transcript has 6 words)"), "{e}");
    let e = err(r#"{"version":1,"title":"x","cuts":{"removeRanges":[{"startS":3,"endS":2,"reason":"r"}]}}"#);
    assert!(e.contains("startS 3 is after endS 2"), "{e}");
    let e = err(r#"{"version":1,"title":"x","cleanup":{"pauses":{"minS":-1}}}"#);
    assert!(e.contains("cleanup.pauses.minS"), "{e}");
    let e = err(r#"{"version":1,"title":"x","markers":[{"name":"m"}],"targetDurationS":0}"#);
    assert!(e.contains("markers[0]: give exactly one") && e.contains("targetDurationS"), "{e}");
    let e = err(r#"{"version":1,"title":"x","cuts":{"keepOnly":[]}}"#);
    assert!(e.contains("keepOnly: is empty"), "{e}");
    let e = err(&format!(r#"{{"version":1,"title":"{}"}}"#, "t".repeat(201)));
    assert!(e.contains("title: longer than 200"), "{e}");

    // numbers built in code can be NaN or infinite
    let mut p = plan(r#"{"version":1,"title":"x"}"#);
    p.cuts.remove_ranges.push(RangeCut { start_s: f64::NAN, end_s: 1.0, reason: "r".into() });
    p.cleanup.silences.push(Span { start_s: 0.0, end_s: f64::INFINITY });
    p.markers.push(PlanMarker { time_s: Some(f64::NEG_INFINITY), word: None, name: "m".into() });
    p.grade = Some(GradePlan { lut_strength: Some(f64::NAN), ..Default::default() });
    p.audio = Some(AudioPlan { target_lufs: Some(f64::INFINITY) });
    p.target_duration_s = Some(f64::NAN);
    let v = compile(&p, &seq(5.0, R), &words(), R).unwrap_err().errors();
    assert_eq!(v.len(), 6, "{v:?}");

    // too many cuts
    let mut p = plan(r#"{"version":1,"title":"x"}"#);
    p.cuts.remove_ranges = vec![RangeCut { start_s: 0.0, end_s: 0.1, reason: "r".into() }; MAX_REMOVALS + 1];
    assert!(compile(&p, &seq(5.0, R), &words(), R).unwrap_err().to_string().contains("limit is 10000"));
}

#[test]
fn word_cuts_take_whole_words_on_frames() {
    let c = run(r#"{"version":1,"title":"x","cuts":{"removeWords":[{"from":1,"reason":"filler"}]}}"#);
    assert_eq!(c.removals.len(), 1);
    let r = &c.removals[0];
    assert_eq!(r.kind, RemovalKind::Word);
    assert_eq!(r.reason, "filler");
    assert_eq!(r.range, TimeRange::from_bounds(s(0.6), s(0.92)), "frame-snapped outward (25 fps)");
    assert_eq!(c.segments, 2);
    assert_no_clipped_words(&c, &words(), s(5.0));
    // reversed indices are fine
    let c2 = run(r#"{"version":1,"title":"x","cuts":{"removeWords":[{"from":4,"to":3,"reason":"r"}]}}"#);
    assert_eq!(c2.removals[0].range.start, s(2.6));
}

#[test]
fn fillers_default_to_the_standard_list() {
    let c = run(r#"{"version":1,"title":"x","cleanup":{"fillers":[]}}"#);
    assert_eq!(c.removals.len(), 1, "{:?}", c.removals);
    assert_eq!(c.removals[0].kind, RemovalKind::Filler);
    assert!(c.removals[0].reason.contains("um"));
    let c = run(r#"{"version":1,"title":"x","cleanup":{"fillers":["speaker here"]}}"#);
    assert_eq!(c.removals[0].range.start, s(3.0));
    assert_eq!(c.removals[0].range.end(), s(4.0));
    assert!(run(r#"{"version":1,"title":"x"}"#).removals.is_empty(), "no fillers unless asked");
}

#[test]
fn ranges_inside_words_grow_to_the_whole_word() {
    // 0.3 is inside "Hello", 0.7 inside "um": both are cut whole
    let c = run(r#"{"version":1,"title":"x","cuts":{"removeRanges":[{"startS":0.3,"endS":0.7,"reason":"r"}]}}"#);
    assert_eq!(c.removals.len(), 1);
    assert_eq!(c.removals[0].range, TimeRange::from_bounds(s(0.2), s(0.92)));
    assert_eq!(c.removals[0].kind, RemovalKind::Range);
    assert!(c.warnings.iter().filter(|w| w.contains("inside a word")).count() == 2, "{:?}", c.warnings);
    assert_no_clipped_words(&c, &words(), s(5.0));
    // clamped to the sequence
    let c = run(r#"{"version":1,"title":"x","cuts":{"removeRanges":[{"startS":-5,"endS":0.1,"reason":"r"},{"startS":4.5,"endS":99,"reason":"r"}]}}"#);
    // boundaries outside words snap outward to frames (25 fps: 0.1 → 0.12, 4.5 → 4.48)
    assert_eq!(c.removals.iter().map(|r| r.range).collect::<Vec<_>>(), [TimeRange::from_bounds(s(0.0), s(0.12)), TimeRange::from_bounds(s(4.48), s(5.0))]);
    assert_eq!(c.after, s(4.36));
    let c = run(r#"{"version":1,"title":"x","cuts":{"removeRanges":[{"startS":7,"endS":9,"reason":"r"}]}}"#);
    assert!(c.removals.is_empty() && c.warnings[0].contains("empty"), "{:?}", c.warnings);
}

#[test]
fn silences_never_take_a_kept_word() {
    // the silence runs over "Hello" and "um": both stay, the gaps go (on frames that keep them whole)
    let c = run(r#"{"version":1,"title":"x","cleanup":{"silences":[{"startS":0.0,"endS":0.95}]}}"#);
    assert_eq!(
        c.removals.iter().map(|r| r.range).collect::<Vec<_>>(),
        [TimeRange::from_bounds(s(0.0), s(0.2)), TimeRange::from_bounds(s(0.52), s(0.6)), TimeRange::from_bounds(s(0.92), s(0.96))]
    );
    assert!(c.warnings.iter().any(|w| w.contains("silence")), "{:?}", c.warnings);
    assert_no_clipped_words(&c, &words(), s(5.0));
    // with "um" removed as a filler, the silence and the filler merge
    let c = run(r#"{"version":1,"title":"x","cleanup":{"fillers":[],"silences":[{"startS":0.5,"endS":1.0}]}}"#);
    assert_eq!(c.removals.len(), 1, "{:?}", c.removals);
    assert_eq!(c.removals[0].range, TimeRange::from_bounds(s(0.52), s(1.0)));
    assert!(c.removals[0].reason.contains("silence") && c.removals[0].reason.contains("filler"));
}

#[test]
fn keep_only_removes_everything_else() {
    let c = run(r#"{"version":1,"title":"x","cuts":{"keepOnly":[{"from":0},{"from":3,"to":5}]}}"#);
    // "um world." and the pauses around them go: from the end of "Hello" to the start of "Second"
    assert_eq!(c.removals.len(), 1, "{:?}", c.removals);
    assert_eq!(c.removals[0].range, TimeRange::from_bounds(s(0.52), s(2.6)));
    assert!(c.removals[0].reason.contains("um world."));
    assert_no_clipped_words(&c, &words(), s(5.0));
    let c = run(r#"{"version":1,"title":"x","cuts":{"keepOnly":[{"from":2,"to":3}]}}"#);
    assert_eq!(c.removals.iter().map(|r| r.range).collect::<Vec<_>>(), [TimeRange::from_bounds(s(0.0), s(1.0)), TimeRange::from_bounds(s(3.0), s(5.0))]);
    assert_eq!(c.segments, 1);
}

#[test]
fn pauses_merges_and_short_fragments() {
    let c = run(r#"{"version":1,"title":"x","cleanup":{"pauses":{"minS":1.0,"keepS":0.1}}}"#);
    assert_eq!(c.removals.len(), 1);
    assert_eq!(c.removals[0].kind, RemovalKind::Pause);
    assert_eq!(c.removals[0].range, TimeRange::from_bounds(s(1.52), s(2.48)));
    // two ranges 0.12 s apart with no word between: the fragment goes too
    let c = run(
        r#"{"version":1,"title":"x","cuts":{"removeRanges":[{"startS":1.5,"endS":1.8,"reason":"a"},{"startS":1.92,"endS":2.4,"reason":"b"},{"startS":2.0,"endS":2.2,"reason":"b"}]}}"#,
    );
    assert_eq!(c.removals.len(), 1, "{:?}", c.removals);
    assert_eq!(c.removals[0].range, TimeRange::from_bounds(s(1.48), s(2.4)));
    assert_eq!(c.removals[0].reason, "a; b");
    assert!(c.warnings.iter().any(|w| w.contains("fragment")), "{:?}", c.warnings);
    // a short fragment holding a word stays
    let c = run(r#"{"version":1,"title":"x","cuts":{"removeWords":[{"from":0,"reason":"a"},{"from":2,"reason":"b"}]}}"#);
    assert_eq!(c.removals.len(), 2, "{:?}", c.removals);
}

#[test]
fn target_duration_and_markers() {
    let c = run(r#"{"version":1,"title":"x","cuts":{"removeWords":[{"from":1,"reason":"r"}]},"targetDurationS":2.0,
        "markers":[{"timeS":3.0,"name":" b "},{"word":2,"name":"a"},{"timeS":0.7,"name":"inside"}]}"#);
    assert!(c.warnings.iter().any(|w| w.contains("4.7 s") && w.contains("2.0 s")), "{:?}", c.warnings);
    let cut = c.removals[0].range.duration;
    assert_eq!(c.markers[0], CompiledMarker { source: s(3.0), at: s(3.0) - cut, name: "b".into() });
    assert_eq!(c.markers[1].at, s(1.0) - cut);
    assert_eq!(c.markers[2].at, s(0.6), "a marker inside a cut lands on the cut");
    assert!(!run(r#"{"version":1,"title":"x","targetDurationS":4.9}"#).warnings.iter().any(|w| w.contains("target")));
}

#[test]
fn words_after_and_removed_texts() {
    let c = run(r#"{"version":1,"title":"x","cuts":{"removeWords":[{"from":1,"reason":"r"}]}}"#);
    let w = words_after(&words(), &c.removals);
    assert_eq!(w.iter().map(|w| w.text.as_str()).collect::<Vec<_>>(), ["Hello", "world.", "Second", "speaker", "here."]);
    assert_eq!(w[1].start, s(1.0) - s(0.32));
    assert_eq!(removed_texts(&words(), &c.removals, 100), ["um"]);
    let all = [Removal { range: TimeRange::from_bounds(s(0.0), s(5.0)), reason: String::new(), kind: RemovalKind::Range }];
    assert_eq!(removed_texts(&words(), &all, 8), ["Hello um…"]);
    assert!(removed_texts(&words(), &[], 8).is_empty());
    assert_eq!(map_time(&c.removals, s(0.0)), s(0.0));
}

#[test]
fn the_documented_example_parses() {
    let doc = include_str!("../../../docs/edit-plans.md");
    let a = doc.find("```json\n").unwrap() + "```json\n".len();
    let b = a + doc[a..].find("\n```").unwrap();
    let p = parse_plan(&doc[a..b]).unwrap();
    assert_eq!(p.title, "Tight interview");
    assert!(validate(&p, 200).is_empty(), "{:?}", validate(&p, 200));
}

#[test]
fn no_words_and_an_empty_sequence() {
    let p = plan(r#"{"version":1,"title":"x","cleanup":{"fillers":[],"pauses":{},"silences":[{"startS":1,"endS":2}]}}"#);
    let c = compile(&p, &seq(5.0, R), &[], R).unwrap();
    assert_eq!(c.removals.len(), 1);
    let c = compile(&p, &Sequence { ..seq(0.0, R) }, &[], FrameRate { num: 0, den: 0 }).unwrap();
    assert!(c.removals.is_empty());
    assert_eq!(c.segments, 0);
}

// ---------------------------------------------------------------------------------------------
// Property test
// ---------------------------------------------------------------------------------------------

fn hostile_f64() -> impl Strategy<Value = f64> {
    prop_oneof![
        // mostly usable (huge values are finite and get clamped); NaN / ∞ fail the whole plan
        60 => -1.0f64..20.0,
        2 => Just(-1e300),
        2 => Just(1e300),
        1 => Just(f64::NAN),
        1 => Just(f64::INFINITY),
    ]
}

/// A word index, now and then out of range.
fn idx(n: usize) -> impl Strategy<Value = usize> {
    prop_oneof![15 => 0..n.max(1), 1 => n..n + 3]
}

fn opt_idx(n: usize) -> impl Strategy<Value = Option<usize>> {
    proptest::option::of(idx(n))
}

prop_compose! {
    fn transcript()(spec in proptest::collection::vec((0u32..5, 0.0f64..0.4, 0.0f64..0.6), 0..24), tail in 0.0f64..2.0)
        -> (Vec<SeqWord>, f64) {
        const T: [&str; 5] = ["um", "uh", "so", "like", "word"];
        let mut t = 0.0;
        let mut out = Vec::new();
        for (i, (k, gap, len)) in spec.into_iter().enumerate() {
            // some words touch, some have zero length
            let a = t + if gap < 0.05 { 0.0 } else { gap };
            let b = a + if len < 0.02 { 0.0 } else { len };
            out.push(word(i, T[k as usize], s(a), s(b)));
            t = b;
        }
        (out, t + tail)
    }
}

fn hostile_plan(n: usize) -> impl Strategy<Value = EditPlan> {
    let words = proptest::collection::vec((idx(n), opt_idx(n)), 0..5);
    let ranges = proptest::collection::vec((hostile_f64(), hostile_f64()), 0..6);
    let spans = proptest::collection::vec((hostile_f64(), hostile_f64()), 0..6);
    let untr = proptest::collection::vec((hostile_f64(), hostile_f64()), 0..3);
    let keep = proptest::option::weighted(0.2, proptest::collection::vec((idx(n), opt_idx(n)), 0..4));
    let pauses = proptest::option::of((-0.5f64..2.0, -0.1f64..0.5));
    let markers = proptest::collection::vec((proptest::option::of(hostile_f64()), opt_idx(n)), 0..3);
    let misc = (any::<bool>(), proptest::option::of(0.1f64..20.0));
    (words, ranges, spans, untr, keep, pauses, markers, misc).prop_map(|(w, r, sp, u, k, pz, m, (fillers, target))| {
        let span = |(a, b): (f64, f64)| Span { start_s: a.min(b), end_s: a.max(b) };
        EditPlan {
            version: 1,
            title: "p".into(),
            rationale: String::new(),
            source: PlanSource::default(),
            output: PlanOutput::default(),
            cuts: Cuts {
                remove_words: w.into_iter().map(|(from, to)| WordCut { from, to, reason: "w".into() }).collect(),
                remove_ranges: r.into_iter().map(|(a, b)| RangeCut { start_s: a.min(b), end_s: a.max(b), reason: "r".into() }).collect(),
                keep_only: k.map(|v| v.into_iter().map(|(from, to)| WordSpan { from, to }).collect()),
            },
            cleanup: Cleanup {
                fillers: fillers.then(Vec::new),
                pauses: pz.map(|(min_s, keep_s)| PauseRule { min_s, keep_s }),
                silences: sp.into_iter().map(span).collect(),
                untranscribed: u.into_iter().map(span).collect(),
            },
            captions: None,
            grade: None,
            audio: None,
            markers: m.into_iter().map(|(time_s, word)| PlanMarker { time_s, word, name: "m".into() }).collect(),
            target_duration_s: target,
            export: None,
        }
    })
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 512, ..ProptestConfig::default() })]

    #[test]
    fn hostile_plans_never_panic_or_clip_kept_words(
        (words, dur, plan, fps) in transcript().prop_flat_map(|(w, d)| {
            let n = w.len();
            (Just(w), Just(d), hostile_plan(n), prop_oneof![Just(FrameRate::FPS_25), Just(FrameRate::FPS_23_976), Just(FrameRate::FPS_29_97), Just(FrameRate { num: 0, den: 1 })])
        })
    ) {
        let q = seq(dur, R);
        let d = q.duration();
        if let Ok(c) = compile(&plan, &q, &words, fps) {
            assert_no_clipped_words(&c, &words, d);
            prop_assert!(c.removals.len() <= MAX_REMOVALS);
            prop_assert!(c.warnings.len() <= MAX_WARNINGS + 1);
            for m in &c.markers {
                prop_assert!(m.at >= Tick::ZERO && m.at <= c.after);
            }
        }
    }
}
