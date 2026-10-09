use super::*;
use serde_json::json;

#[test]
fn aspect_frames_keep_the_short_side() {
    assert_eq!(Aspect::Vertical.frame_for(1920, 1080), (1080, 1920));
    assert_eq!(Aspect::Square.frame_for(1920, 1080), (1080, 1080));
    assert_eq!(Aspect::Portrait.frame_for(1920, 1080), (1080, 1350));
    assert_eq!(Aspect::Wide.frame_for(1920, 1080), (1920, 1080));
    assert_eq!(Aspect::Wide.frame_for(1080, 1920), (1920, 1080));
    assert_eq!(Aspect::Vertical.frame_for(3840, 2160), (2160, 3840));
    // hostile sizes stay within the limits and even
    for (w, h) in [(0, 0), (1, 1), (u32::MAX, u32::MAX), (MAX_FRAME_SIDE, MAX_FRAME_SIDE), (u32::MAX, 3), (7, 7)] {
        for a in [Aspect::Wide, Aspect::Vertical, Aspect::Square, Aspect::Portrait] {
            let (fw, fh) = a.frame_for(w, h);
            assert!(filmcraft_project::validate_frame_size(fw, fh).is_ok(), "{a:?} {w}x{h} → {fw}x{fh}");
            assert!(fw % 2 == 0 && fh % 2 == 0);
        }
    }
}

#[test]
fn caption_chars_follow_the_frame() {
    assert_eq!(caption_chars_for(1920, 1080, 54.0), 42);
    let v = caption_chars_for(1080, 1920, 54.0);
    assert!((14..=22).contains(&v), "{v}");
    assert!(caption_chars_for(1080, 1080, 54.0) > v);
    assert_eq!(caption_chars_for(0, 0, f32::NAN), caption_chars_for(1, 1, 4.0));
    assert!((8..=42).contains(&caption_chars_for(u32::MAX, 1, f32::INFINITY)));
}

#[test]
fn style_object_maps_onto_the_track_style() {
    let base = CaptionStyle::default();
    let l = caption_look(
        &json!({"size": 0.06, "color": "#ffdd00", "background": false, "outline": 3, "outlineColor": [0, 0, 0],
                "position": "middle", "align": "left", "case": "upper", "font": "Inter", "margin": 0.1, "lineSpacing": 1.1}),
        &base,
    );
    assert!(l.warnings.is_empty(), "{:?}", l.warnings);
    assert!((l.style.size - 64.8).abs() < 0.01);
    assert_eq!(l.style.color, [255, 221, 0, 255]);
    assert!(!l.style.background);
    assert_eq!(l.style.outline, 3.0);
    assert_eq!(l.style.anchor, CaptionAnchor::Middle);
    assert_eq!(l.style.align, CaptionAlign::Left);
    assert_eq!(l.case, Some(TextCase::Upper));
    assert_eq!(caption_look(&json!({"size": 72}), &base).style.size, 72.0);
    let boxed = caption_look(&json!({"backgroundColor": "#000000cc"}), &base).style;
    assert!(boxed.background && boxed.background_color == [0, 0, 0, 0xcc]);
    assert_eq!(caption_look(&json!({"color": "#fff"}), &base).style.color, [255, 255, 255, 255]);
}

#[test]
fn hostile_style_values_are_warnings() {
    let base = CaptionStyle::default();
    let huge = "x".repeat(1 << 20);
    let mut many = serde_json::Map::new();
    for i in 0..1000 {
        many.insert(format!("k{i}"), json!(i));
    }
    for v in [
        json!({"size": f64::MAX, "color": "#zzzzzz", "outline": -1, "margin": 9, "case": "shouty", "position": 3}),
        json!({"font": huge.clone(), "color": huge.clone()}),
        Value::Object(std::iter::once((huge.clone(), json!(1))).collect()),
        json!({"color": [1, 2], "background": "nope", "lineSpacing": "x", "highlightColor": "#ff0"}),
        json!({"color": "#é1é1é1"}),
        json!({"color": [300, -1, 0.5]}),
        Value::Object(many),
        json!("big"),
        json!([1, 2, 3]),
        json!(42),
    ] {
        let l = caption_look(&v, &base);
        assert!(!l.warnings.is_empty(), "{v}");
        assert!(l.warnings.len() <= MAX_STYLE_KEYS + 1);
        assert!(l.warnings.iter().all(|w| w.chars().count() < 300), "warnings echo at most a short excerpt");
    }
    // nothing valid in the hostile ones: the style is unchanged
    assert_eq!(caption_look(&json!({"size": -3, "color": "nah"}), &base).style, base);
    assert!(caption_look(&Value::Null, &base).warnings.is_empty());
}

#[test]
fn text_case() {
    assert_eq!(apply_case("hello world.", TextCase::Upper), "HELLO WORLD.");
    assert_eq!(apply_case("Hello World", TextCase::Lower), "hello world");
    assert_eq!(apply_case("don't stop  me\nnow 42x", TextCase::Title), "Don't Stop  Me\nNow 42X");
    assert_eq!(apply_case("", TextCase::Title), "");
    assert_eq!(apply_case("straße", TextCase::Upper), "STRASSE");
}
