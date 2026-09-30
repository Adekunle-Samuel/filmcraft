//! Oracle-backed tests (skipped when ffmpeg is unavailable).

use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;

use filmcraft_media::FrameRequest;
use filmcraft_time::{TICKS_PER_SECOND, Tick};

fn ffmpeg() -> Option<&'static str> {
    ["/opt/homebrew/bin/ffmpeg", "/usr/local/bin/ffmpeg", "/usr/bin/ffmpeg"].into_iter().find(|p| std::path::Path::new(p).exists())
}

fn fixture(name: &str, args: &[&str]) -> Option<Arc<[u8]>> {
    let ff = ffmpeg().or_else(|| {
        eprintln!("ffmpeg not found; skipping");
        None
    })?;
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/fixtures/codecs");
    std::fs::create_dir_all(&dir).ok()?;
    let out = dir.join(name);
    if !out.exists() {
        let st = Command::new(ff).args(["-y", "-v", "error"]).args(args).arg(&out).status().ok()?;
        if !st.success() {
            eprintln!("fixture {name} failed; skipping");
            return None;
        }
    }
    Some(std::fs::read(out).ok()?.into())
}

#[test]
fn mjpeg_mov_with_pcm() {
    let Some(b) = fixture(
        "red_mjpeg.mov",
        &[
            "-f",
            "lavfi",
            "-i",
            "color=c=red:s=320x240:r=24:d=2",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=1000:sample_rate=48000:d=2",
            "-c:v",
            "mjpeg",
            "-q:v",
            "2",
            "-c:a",
            "pcm_s16le",
            "-shortest",
        ],
    ) else {
        return;
    };
    let src = crate::open_bytes("red_mjpeg.mov", b).unwrap();
    let info = src.info().clone();
    assert_eq!(info.video.as_ref().unwrap().width, 320);
    assert_eq!(info.video.as_ref().unwrap().frame_rate, filmcraft_time::FrameRate::FPS_24);
    let f = src.video_frame(FrameRequest::full(Tick(TICKS_PER_SECOND))).unwrap();
    let px = f.to_rgba8();
    let c = &px[(120 * 320 + 160) * 4..][..3];
    assert!(c[0] > 230 && c[1] < 30 && c[2] < 30, "{c:?}");
    let a = src.audio(0, 4800, 48_000).unwrap();
    let peak = a.peaks()[0];
    assert!(peak > 0.1, "{peak}");
}

#[test]
fn aac_mp4_decodes() {
    let Some(b) = fixture("tone_aac.mp4", &["-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000:d=3", "-c:a", "aac", "-b:a", "128k"]) else { return };
    let src = crate::open_bytes("tone_aac.mp4", b).unwrap();
    assert!(src.info().audio.as_ref().unwrap().codec.contains("AAC"));
    // read in the middle (random access) and sequentially
    let a = src.audio(48_000, 4800, 48_000).unwrap();
    let p = a.peaks()[0];
    assert!(p > 0.05 && p < 1.0, "{p}");
    let b2 = src.audio(48_000 + 4800, 4800, 44_100).unwrap();
    assert!(b2.peaks()[0] > 0.05);
}

#[test]
fn mp3_file_decodes() {
    let Some(b) = fixture("tone.mp3", &["-f", "lavfi", "-i", "sine=frequency=440:sample_rate=44100:d=2", "-c:a", "libmp3lame", "-b:a", "192k"]) else { return };
    let src = crate::open_bytes("tone.mp3", b).unwrap();
    assert!((src.info().duration.seconds() - 2.0).abs() < 0.2, "{}", src.info().duration.seconds());
    let a = src.audio(44_100, 4410, 44_100).unwrap();
    assert!(a.peaks()[0] > 0.1);
}

#[test]
fn seek_backwards_and_forwards_mjpeg() {
    let Some(b) = fixture("counter_mjpeg.mov", &["-f", "lavfi", "-i", "testsrc2=s=160x120:r=25:d=3", "-c:v", "mjpeg"]) else { return };
    let src = crate::open_bytes("counter_mjpeg.mov", b).unwrap();
    let rate = src.info().frame_rate();
    let a = src.video_frame(FrameRequest::full(rate.tick_of(60))).unwrap();
    let b = src.video_frame(FrameRequest::full(rate.tick_of(10))).unwrap();
    let c = src.video_frame(FrameRequest::full(rate.tick_of(60))).unwrap();
    assert_eq!(a.to_rgba8(), c.to_rgba8());
    assert_ne!(a.to_rgba8(), b.to_rgba8());
}

#[test]
fn h264_mp4_decodes_and_seeks() {
    let Some(b) = fixture(
        "mandel_h264.mp4",
        &["-f", "lavfi", "-i", "mandelbrot=s=640x360:r=25", "-t", "4", "-c:v", "libx264", "-preset", "fast", "-bf", "3", "-g", "25", "-pix_fmt", "yuv420p"],
    ) else {
        return;
    };
    let src = crate::open_bytes("mandel_h264.mp4", b).unwrap();
    assert!(src.info().video.as_ref().unwrap().codec.contains("H.264"));
    let rate = src.info().frame_rate();
    let late = src.video_frame(FrameRequest::full(rate.tick_of(70))).unwrap();
    assert_eq!((late.width, late.height), (640, 360));
    let early = src.video_frame(FrameRequest::full(rate.tick_of(3))).unwrap();
    let again = src.video_frame(FrameRequest::full(rate.tick_of(70))).unwrap();
    assert_eq!(late.to_rgba8(), again.to_rgba8(), "random access is deterministic");
    assert_ne!(late.to_rgba8(), early.to_rgba8());
    // sequential access after a seek
    for f in 71..90 {
        src.video_frame(FrameRequest::full(rate.tick_of(f))).unwrap();
    }
}

#[test]
fn prores_mov_decodes() {
    let Some(b) = fixture("bars_prores.mov", &["-f", "lavfi", "-i", "smptehdbars=s=640x360:r=24:d=1", "-c:v", "prores_ks", "-profile:v", "3"]) else { return };
    let src = crate::open_bytes("bars_prores.mov", b).unwrap();
    assert!(src.info().video.as_ref().unwrap().codec.contains("ProRes"));
    let f = src.video_frame(FrameRequest::full(Tick(TICKS_PER_SECOND / 2))).unwrap();
    assert!(f.format_label().contains("4:2:2"));
    let px = f.to_rgba8();
    // leftmost bars area is 40% grey
    let c = &px[(100 * 640 + 20) * 4..][..3];
    assert!((c[0] as i32 - 104).abs() < 6, "{c:?}");
}
