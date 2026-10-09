//! `media.renderFrame` / `media.contactSheet` on the demo project (procedural footage).

use filmcraft_project::{ItemId, ItemKind};
use serde_json::{Value, json};

use super::*;
use crate::Session;

const PNG_MAGIC: &[u8] = b"\x89PNG\r\n\x1a\n";

fn demo() -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s
}

/// The first media item with a picture, and the audio-only one.
fn media_items(s: &Session) -> (ItemId, ItemId) {
    let mut video = None;
    let mut audio = None;
    for (id, it) in &s.project.items {
        if let ItemKind::Media(m) = &it.kind {
            if m.info.video.is_some() && video.is_none_or(|v: ItemId| id.0 < v.0) {
                video = Some(*id);
            }
            if m.info.video.is_none() {
                audio = Some(*id);
            }
        }
    }
    (video.unwrap(), audio.unwrap())
}

/// Decode the base64 PNG of a result and check it against `width` / `height`.
fn png_of(v: &Value) -> image::RgbaImage {
    let b64 = v["png"].as_str().unwrap();
    let bytes = unbase64(b64);
    assert!(bytes.starts_with(PNG_MAGIC), "not a PNG");
    let img = image::load_from_memory(&bytes).unwrap().to_rgba8();
    assert_eq!((img.width() as u64, img.height() as u64), (v["width"].as_u64().unwrap(), v["height"].as_u64().unwrap()));
    img
}

fn unbase64(s: &str) -> Vec<u8> {
    const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::new();
    let mut acc = 0u32;
    let mut bits = 0;
    for c in s.bytes().filter(|c| *c != b'=') {
        acc = (acc << 6) | A.iter().position(|a| *a == c).unwrap() as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    out
}

/// Nothing an agent could notice changes: playheads, selections, history, journal, revision.
fn snapshot(s: &Session) -> Value {
    json!({
        "playheads": s.state.playheads.iter().map(|(k, v)| (k.0, v.0)).collect::<Vec<_>>(),
        "source": s.state.source_playhead.0,
        "selection": s.state.selection.iter().map(|c| c.0).collect::<Vec<_>>(),
        "projectSelection": s.state.project_selection.iter().map(|i| i.0).collect::<Vec<_>>(),
        "undo": s.history.undo.len(),
        "journal": s.journal.len(),
        "revision": s.revision,
    })
}

#[test]
fn base64_matches_rfc4648() {
    for (raw, enc) in [("", ""), ("f", "Zg=="), ("fo", "Zm8="), ("foo", "Zm9v"), ("foob", "Zm9vYg=="), ("fooba", "Zm9vYmE="), ("foobar", "Zm9vYmFy")] {
        assert_eq!(base64(raw.as_bytes()), enc);
        assert_eq!(unbase64(enc), raw.as_bytes());
    }
}

#[test]
fn resize_refuses_a_buffer_of_the_wrong_size() {
    assert!(resize(&[0; 12], 2, 2, 1, 1).is_none());
    assert!(resize(&[0; 16], 2, 2, 0, 1).is_none());
    let one = resize(&[10, 20, 30, 255, 30, 40, 50, 255, 10, 20, 30, 255, 30, 40, 50, 255], 2, 2, 1, 1).unwrap();
    assert_eq!(one, vec![20, 30, 40, 255]);
    // upscaling repeats pixels
    assert_eq!(resize(&[1, 2, 3, 4], 1, 1, 2, 1).unwrap(), vec![1, 2, 3, 4, 1, 2, 3, 4]);
}

#[test]
fn render_frame_of_the_sequence_leaves_the_session_alone() {
    let mut s = demo();
    s.execute("playhead.set", json!({"seconds": 1.0})).unwrap();
    s.execute("sequence.selectionFollowsPlayhead", json!({"on": true})).unwrap();
    let before = snapshot(&s);
    let a = s.execute("media.renderFrame", json!({"seconds": 2.0, "maxSide": 320})).unwrap();
    let img = png_of(&a);
    assert_eq!((img.width(), img.height()), (320, 180));
    assert!((a["seconds"].as_f64().unwrap() - 2.0).abs() < 0.05, "{}", a["seconds"]);
    let b = s.execute("media.renderFrame", json!({"sequence": true, "seconds": 12.0, "maxSide": 320})).unwrap();
    assert_ne!(a["png"], b["png"], "different times give different frames");
    assert_eq!(snapshot(&s), before, "a query must not move the playhead, select, journal or add undo steps");
    assert!(!crate::commands::find("media.renderFrame").unwrap().journal);
}

#[test]
fn render_frame_of_a_media_item() {
    let mut s = demo();
    let (video, _) = media_items(&s);
    let before = snapshot(&s);
    let v = s.execute("media.renderFrame", json!({"item": video.0, "seconds": 1.5, "maxSide": 400})).unwrap();
    let img = png_of(&v);
    assert!(img.width().max(img.height()) <= 400 && img.width() > img.height());
    assert_eq!(v["item"], video.0);
    // not black: the demo scenes are colourful
    assert!(img.pixels().any(|p| p.0[0] > 40 || p.0[1] > 40 || p.0[2] > 40));
    // the default size stays under the vision cap and never upscales
    let v = s.execute("media.renderFrame", json!({"item": video.0, "seconds": 0})).unwrap();
    assert!(v["width"].as_u64().unwrap() <= u64::from(DEFAULT_SIDE));
    assert_eq!(snapshot(&s), before);
}

#[test]
fn contact_sheet_defaults_and_caps() {
    let mut s = demo();
    let before = snapshot(&s);
    let v = s.execute("media.contactSheet", json!({})).unwrap();
    png_of(&v);
    let times: Vec<f64> = v["times"].as_array().unwrap().iter().map(|t| t.as_f64().unwrap()).collect();
    assert_eq!(times.len(), DEFAULT_FRAMES);
    assert!(times.windows(2).all(|w| w[0] < w[1]), "{times:?}");
    let (cols, rows) = (v["cols"].as_u64().unwrap(), v["rows"].as_u64().unwrap());
    assert!(cols * rows >= DEFAULT_FRAMES as u64 && (cols - 1) * rows < DEFAULT_FRAMES as u64 + rows, "{cols}x{rows}");
    assert!(v["width"].as_u64().unwrap() <= u64::from(DEFAULT_SIDE) && v["height"].as_u64().unwrap() <= u64::from(DEFAULT_SIDE));
    // the most frames at the largest size stay within MAX_SIDE²
    let v = s.execute("media.contactSheet", json!({"sequence": true, "count": 48, "maxSide": 1e9})).unwrap();
    png_of(&v);
    assert_eq!(v["times"].as_array().unwrap().len(), MAX_FRAMES);
    assert!(v["width"].as_u64().unwrap() <= u64::from(MAX_SIDE) && v["height"].as_u64().unwrap() <= u64::from(MAX_SIDE));
    assert_eq!(snapshot(&s), before);
}

#[test]
fn contact_sheet_of_given_times_on_an_item() {
    let mut s = demo();
    let (video, _) = media_items(&s);
    let v = s.execute("media.contactSheet", json!({"item": video.0, "times": [0, 1, 2, 1e300], "cols": 9, "maxSide": 600})).unwrap();
    png_of(&v);
    // cols past the frame count shrink to it; a time past the end is the last frame
    assert_eq!((v["cols"].as_u64(), v["rows"].as_u64()), (Some(4), Some(1)));
    let times: Vec<f64> = v["times"].as_array().unwrap().iter().map(|t| t.as_f64().unwrap()).collect();
    let dur = s.project.item(video).unwrap().duration().seconds();
    assert!(times[3] < dur && times[3] > dur - 0.1, "{times:?} vs {dur}");
    assert_eq!(v["width"].as_u64(), Some(600));
}

#[test]
fn hostile_parameters_are_errors_not_panics() {
    let mut s = demo();
    let (video, audio) = media_items(&s);
    let before = snapshot(&s);
    for p in [
        json!({}),
        json!({"seconds": -1}),
        json!({"seconds": "NaN"}),
        json!({"seconds": null}),
        json!({"seconds": 1, "maxSide": -5}),
        json!({"seconds": 1, "maxSide": "big"}),
        json!({"seconds": 1, "item": 999_999}),
        json!({"seconds": 1, "item": -3}),
        json!({"seconds": 1, "item": "seven"}),
        json!({"seconds": 1, "item": audio.0}),
        json!({"seconds": 1, "item": video.0, "sequence": true}),
        json!({"seconds": 1, "sequence": "yes"}),
    ] {
        let r = s.execute("media.renderFrame", p.clone());
        assert!(r.is_err(), "renderFrame {p} should fail");
    }
    for p in [
        json!({"count": 0}),
        json!({"count": 49}),
        json!({"count": 1e9}),
        json!({"count": -1}),
        json!({"count": 2.5}),
        json!({"count": "12"}),
        json!({"times": []}),
        json!({"times": vec![1.0; 49]}),
        json!({"times": vec![1.0; 100_000]}),
        json!({"times": ["NaN"]}),
        json!({"times": [-1]}),
        json!({"times": [null]}),
        json!({"times": 3}),
        json!({"cols": 0}),
        json!({"cols": 1e12}),
        json!({"maxSide": 0}),
        json!({"count": 48, "maxSide": 16}),
        json!({"item": audio.0}),
    ] {
        let r = s.execute("media.contactSheet", p.clone());
        assert!(r.is_err(), "contactSheet {p} should fail");
    }
    // tiny but legal sizes are raised to the minimum, not refused
    let v = s.execute("media.renderFrame", json!({"seconds": 1e300, "maxSide": 1})).unwrap();
    assert_eq!(v["width"].as_u64(), Some(u64::from(MIN_SIDE)));
    assert_eq!(snapshot(&s), before);
}

#[test]
fn without_a_sequence_the_commands_say_so() {
    let mut s = Session::default();
    let e = s.execute("media.renderFrame", json!({"seconds": 0})).unwrap_err();
    assert!(matches!(e, crate::EngineError::NoSequence), "{e}");
    let e = s.execute("media.contactSheet", json!({})).unwrap_err();
    assert!(matches!(e, crate::EngineError::NoSequence), "{e}");
    // an empty sequence renders (black) but has nothing to sample
    s.execute("file.newSequence", json!({"name": "Empty"})).unwrap();
    let v = s.execute("media.renderFrame", json!({"seconds": 3, "maxSide": 64})).unwrap();
    png_of(&v);
    assert!(s.execute("media.contactSheet", json!({})).is_err());
}
