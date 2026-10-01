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

/// Solid-colour Matroska fixtures: decoded frames must match the colour, audio must be a 1 kHz tone.
fn check_mkv(name: &str, vcodec: &[&str], acodec: &[&str]) {
    let mut args: Vec<&str> =
        vec!["-f", "lavfi", "-i", "color=c=0x3060c0:s=320x240:r=25:d=2", "-f", "lavfi", "-i", "sine=frequency=1000:sample_rate=48000:d=2"];
    args.extend_from_slice(vcodec);
    args.extend_from_slice(acodec);
    let Some(b) = fixture(name, &args) else { return };
    let src = crate::open_bytes(name, b).expect("open");
    let info = src.info();
    assert_eq!(info.container, "Matroska");
    let v = info.video.as_ref().expect("video");
    assert_eq!((v.width, v.height), (320, 240));
    assert_eq!(v.frame_rate.num as f64 / v.frame_rate.den as f64, 25.0);
    let d = info.duration.0 as f64 / TICKS_PER_SECOND as f64;
    assert!((d - 2.0).abs() < 0.1, "duration {d}");
    for secs in [0.0, 1.24, 0.4] {
        let f = src.video_frame(FrameRequest { time: Tick((secs * TICKS_PER_SECOND as f64) as i64), scale: 1.0 }).expect("frame");
        let rgba = f.to_rgba8();
        let px = &rgba[(120 * 320 + 160) * 4..][..3];
        for (got, want) in px.iter().zip([0x30u8, 0x60, 0xc0]) {
            assert!((*got as i32 - want as i32).abs() <= 6, "{name} at {secs}s: {px:?}");
        }
    }
    let a = src.audio(24_000, 4800, 48_000).expect("audio");
    let peak = a.channels[0].iter().fold(0f32, |m, s| m.max(s.abs()));
    assert!(peak > 0.05 && peak < 1.0, "{name}: audio peak {peak}");
}

#[test]
fn matroska_h264_aac() {
    check_mkv("blue_h264_aac.mkv", &["-c:v", "libx264", "-bf", "2", "-pix_fmt", "yuv420p"], &["-c:a", "aac"]);
}

#[test]
fn matroska_hevc_flac() {
    check_mkv("blue_hevc_flac.mkv", &["-c:v", "libx265", "-x265-params", "log-level=error", "-pix_fmt", "yuv420p"], &["-c:a", "flac"]);
}

#[test]
fn matroska_prores_pcm() {
    check_mkv("blue_prores_pcm.mkv", &["-c:v", "prores_ks", "-profile:v", "2"], &["-c:a", "pcm_s16le"]);
}

/// ffmpeg + libopus decode of a container fixture (pre-skip / codec delay / edit list applied) as
/// interleaved f32 at 48 kHz: the reference for our Opus path.
fn opus_reference(name: &str) -> Option<Vec<f32>> {
    let ff = ffmpeg()?;
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/fixtures/codecs");
    let out = dir.join(format!("{name}.ref.f32"));
    let st = Command::new(ff)
        .args(["-y", "-v", "error", "-c:a", "libopus", "-i"])
        .arg(dir.join(name))
        .args(["-f", "f32le", "-ar", "48000"])
        .arg(&out)
        .status()
        .ok()?;
    if !st.success() {
        return None;
    }
    Some(std::fs::read(out).ok()?.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect())
}

fn snr_db(reference: &[f32], test: &[f32]) -> f64 {
    let (mut s, mut e) = (0f64, 0f64);
    for (r, t) in reference.iter().zip(test) {
        s += (*r as f64).powi(2);
        e += (*r as f64 - *t as f64).powi(2);
    }
    if e == 0.0 { 200.0 } else { 10.0 * (s.max(1e-20) / e).log10() }
}

/// Interleaves `frames` of `src.audio` from `start` (48 kHz).
fn read_interleaved(src: &filmcraft_media::SharedSource, start: i64, frames: usize) -> Vec<f32> {
    let a = src.audio(start, frames, 48_000).expect("audio");
    let ch = a.channels.len();
    let mut v = vec![0f32; frames * ch];
    for (c, chan) in a.channels.iter().enumerate() {
        for (i, s) in chan.iter().enumerate() {
            v[i * ch + c] = *s;
        }
    }
    v
}

/// Opus in a container: metadata, sample-exact alignment (pre-skip honoured) against libopus for a
/// sequential read, and the same samples after random access (pre-roll).
fn check_opus(name: &str, channels: usize, expr: &str, extra: &[&str], container: &str) {
    let mut args: Vec<&str> = vec!["-f", "lavfi", "-i", expr, "-t", "3", "-c:a", "libopus"];
    args.extend_from_slice(extra);
    let Some(b) = fixture(name, &args) else { return };
    let Some(reference) = opus_reference(name) else {
        eprintln!("libopus decoder unavailable; skipping {name}");
        return;
    };
    let src = crate::open_bytes(name, b).expect("open");
    let info = src.info().clone();
    assert_eq!(info.container, container);
    let a = info.audio.as_ref().expect("audio");
    assert_eq!((a.codec.as_str(), a.sample_rate, a.channels as usize), ("Opus", 48_000, channels));
    let d = info.duration.seconds();
    assert!((d - 3.0).abs() < 0.03, "{name}: duration {d}");
    let total = reference.len() / channels;
    assert!((total as i64 - 144_000).abs() < 960, "{name}: reference length {total}");

    // Sequential read in 100 ms blocks.
    let mut ours = Vec::with_capacity(reference.len());
    let mut pos = 0;
    while pos < total {
        let n = 4800.min(total - pos);
        ours.extend(read_interleaved(&src, pos as i64, n));
        pos += n;
    }
    let snr = snr_db(&reference, &ours);
    eprintln!("{name}: sequential SNR vs libopus {snr:.1} dB");
    assert!(snr > 40.0, "{name}: sequential SNR {snr:.1} dB");

    // Random access on a fresh source (cold decoder): 100 ms at 1.7 s, then back to 0.5 s.
    let src = crate::open_bytes(name, fixture(name, &args).expect("fixture")).expect("open");
    for start in [81_600usize, 24_000] {
        let got = read_interleaved(&src, start as i64, 4800);
        let want = &reference[start * channels..(start + 4800) * channels];
        let snr = snr_db(want, &got);
        eprintln!("{name}: random access at {start}: SNR {snr:.1} dB");
        assert!(snr > 40.0, "{name}: random access at {start}: SNR {snr:.1} dB");
    }
}

#[test]
fn opus_webm_stereo() {
    check_opus(
        "tones_opus.webm",
        2,
        "aevalsrc=0.4*sin(2*PI*440*t)+0.1*sin(2*PI*5000*t)|0.3*sin(2*PI*660*t)+0.1*sin(2*PI*3100*t):s=48000",
        &["-b:a", "128k"],
        "WebM",
    );
}

#[test]
fn opus_mkv_mono_speechlike_16k() {
    // Low bitrate VoIP mode exercises SILK/hybrid; the input rate is 16 kHz but Opus still outputs 48 kHz.
    check_opus("tones_opus_voip.mkv", 1, "aevalsrc=0.4*sin(2*PI*220*t)*(0.6+0.4*sin(2*PI*3*t)):s=16000", &["-b:a", "16k", "-application", "voip"], "Matroska");
}

#[test]
fn opus_mkv_surround_51() {
    check_opus(
        "tones_opus_51.mkv",
        6,
        "aevalsrc=0.3*sin(2*PI*300*t)|0.3*sin(2*PI*400*t)|0.3*sin(2*PI*500*t)|0.3*sin(2*PI*60*t)|0.3*sin(2*PI*700*t)|0.3*sin(2*PI*800*t):s=48000:c=5.1",
        &["-b:a", "256k"],
        "Matroska",
    );
}

#[test]
fn opus_mp4_stereo() {
    check_opus(
        "tones_opus.mp4",
        2,
        "aevalsrc=0.4*sin(2*PI*440*t)+0.1*sin(2*PI*5000*t)|0.3*sin(2*PI*660*t)+0.1*sin(2*PI*3100*t):s=48000",
        &["-b:a", "128k"],
        "MPEG-4",
    );
}
