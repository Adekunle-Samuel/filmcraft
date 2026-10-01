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

/// Path of a fixture under `target/fixtures/codecs/`, generated with ffmpeg when missing
/// (`None` when ffmpeg or the encoder is unavailable).
fn fixture_path(name: &str, args: &[&str]) -> Option<PathBuf> {
    fixture(name, args)?;
    Some(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/fixtures/codecs").join(name))
}

/// ffmpeg's decode of `src` as raw planar video (the oracle).
fn reference_yuv(src: &std::path::Path, pix_fmt: &str) -> Option<Vec<u8>> {
    let name = format!("{}.{pix_fmt}.yuv", src.file_name()?.to_str()?);
    let path = src.to_str()?;
    let b = fixture(&name, &["-i", path, "-fps_mode", "passthrough", "-f", "rawvideo", "-pix_fmt", pix_fmt])?;
    Some(b.to_vec())
}

/// Planes of a decoded YUV frame as little-endian bytes (ffmpeg rawvideo layout).
fn yuv_bytes(f: &filmcraft_frame::VideoFrame) -> Vec<u8> {
    match &f.data {
        filmcraft_frame::PixelData::Yuv8 { planes, .. } => planes.iter().flat_map(|p| p.iter().copied()).collect(),
        filmcraft_frame::PixelData::Yuv16 { planes, .. } => planes.iter().flat_map(|p| p.iter().flat_map(|s| s.to_le_bytes())).collect(),
        _ => panic!("not a YUV frame: {}", f.format_label()),
    }
}

/// Random access into a VP9 file: frames requested out of order (seeks back and forth across key
/// frames, then sequential playback) are sample-exact against ffmpeg's decode.
fn check_vp9_seeks(name: &str, container: &str, pix_fmt: &str, extra: &[&str], frames: usize) {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/fixtures/codecs");
    let log = dir.join(format!("{name}.passlog")).to_string_lossy().into_owned();
    let mut args = vec!["-f", "lavfi", "-i", "testsrc2=s=352x288:r=25,noise=alls=8:allf=t", "-frames:v"];
    let n = frames.to_string();
    args.push(&n);
    args.extend_from_slice(&["-c:v", "libvpx-vp9", "-pix_fmt", pix_fmt, "-g", "25", "-b:v", "600k"]);
    args.extend_from_slice(extra);
    if extra.contains(&"-auto-alt-ref") {
        // libvpx only uses alternate reference frames in two-pass mode: run the first pass.
        let Some(ff) = ffmpeg() else { return };
        if !dir.join(name).exists() {
            let _ = std::fs::create_dir_all(&dir);
            let ok = Command::new(ff).args(["-y", "-v", "error"]).args(&args).args(["-pass", "1", "-passlogfile"]).arg(&log).args(["-f", "null", "-"]).status();
            if !ok.is_ok_and(|s| s.success()) {
                eprintln!("first pass for {name} failed; skipping");
                return;
            }
        }
        args.extend_from_slice(&["-pass", "2", "-passlogfile"]);
        args.push(&log);
    }
    let Some(path) = fixture_path(name, &args) else { return };
    let Some(reference) = reference_yuv(&path, pix_fmt) else { return };
    let bytes: Arc<[u8]> = std::fs::read(&path).unwrap().into();
    let src = crate::open_bytes(name, bytes).unwrap();
    let info = src.info().clone();
    assert_eq!(info.container, container);
    let v = info.video.as_ref().unwrap();
    assert!(v.codec.contains("VP9"), "{}", v.codec);
    assert_eq!((v.width, v.height), (352, 288));
    let frame_len = reference.len() / frames;
    let rate = info.frame_rate();
    let order: Vec<usize> = [frames - 3, 3, 40, 26, 24, frames - 1, 0, 12].into_iter().chain(30..45).filter(|&k| k < frames).collect();
    for k in order {
        let f = src.video_frame(FrameRequest::full(rate.tick_of(k as i64))).unwrap();
        assert_eq!((f.width, f.height), (352, 288));
        let got = yuv_bytes(&f);
        assert!(got == reference[k * frame_len..(k + 1) * frame_len], "{name}: frame {k} differs from ffmpeg");
    }
}

#[test]
fn webm_vp9_seeks_bit_exact() {
    check_vp9_seeks("noise_vp9.webm", "WebM", "yuv420p", &["-deadline", "realtime", "-speed", "8"], 60);
}

#[test]
fn webm_vp9_altref_superframes_seek_bit_exact() {
    // Good-quality encode with alternate reference frames: hidden frames packed into superframes.
    check_vp9_seeks("noise_vp9_altref.webm", "WebM", "yuv420p", &["-speed", "4", "-auto-alt-ref", "1", "-lag-in-frames", "16"], 60);
}

#[test]
fn mp4_vp9_10bit_seeks_bit_exact() {
    check_vp9_seeks("noise_vp9_10bit.mp4", "MPEG-4", "yuv420p10le", &["-profile:v", "2", "-deadline", "realtime", "-speed", "8"], 50);
}

#[test]
fn mkv_vp9_444_12bit_seeks_bit_exact() {
    check_vp9_seeks("noise_vp9_444_12bit.mkv", "Matroska", "yuv444p12le", &["-profile:v", "3", "-deadline", "realtime", "-speed", "8"], 30);
}

#[test]
fn mkv_vp9_422_seeks_bit_exact() {
    check_vp9_seeks("noise_vp9_422.mkv", "Matroska", "yuv422p", &["-profile:v", "1", "-deadline", "realtime", "-speed", "8"], 30);
}

/// Every sample flagged as sync (as in an MP4 without `stss`): the GOP cache must still start
/// decoding at a real VP9 key frame.
#[test]
fn vp9_random_access_ignores_bogus_sync_flags() {
    use crate::gop::{GopCache, VideoSamples};
    let Some(path) = fixture_path(
        "noise_vp9_g20.ivf",
        &["-f", "lavfi", "-i", "testsrc2=s=176x144:r=25", "-frames:v", "50", "-c:v", "libvpx-vp9", "-g", "20", "-deadline", "realtime", "-speed", "8"],
    ) else {
        return;
    };
    let Some(reference) = reference_yuv(&path, "yuv420p") else { return };
    let data = std::fs::read(&path).unwrap();
    let mut chunks = Vec::new();
    let mut p = 32;
    while p + 12 <= data.len() {
        let sz = u32::from_le_bytes(data[p..p + 4].try_into().unwrap()) as usize;
        chunks.push(data[p + 12..p + 12 + sz].to_vec());
        p += 12 + sz;
    }
    struct AllSync(Vec<Vec<u8>>);
    impl VideoSamples for AllSync {
        fn count(&self) -> usize {
            self.0.len()
        }
        fn pts(&self, i: usize) -> i64 {
            i as i64
        }
        fn sync_before(&self, i: usize) -> usize {
            i
        }
        fn sample_at(&self, t: i64) -> Option<usize> {
            (t >= 0 && (t as usize) < self.0.len()).then_some(t as usize)
        }
        fn read(&self, i: usize) -> crate::Result<Vec<u8>> {
            Ok(self.0[i].clone())
        }
        fn make_decoder(&self) -> crate::Result<Box<dyn crate::VideoDecoder>> {
            Ok(Box::new(crate::video::Vp9Decoder::new(None)))
        }
    }
    let s = AllSync(chunks);
    let cache = GopCache::new(None);
    let frame_len = 176 * 144 * 3 / 2;
    for k in [37usize, 5, 49, 21, 20, 19] {
        let f = cache.frame(&s, k as i64).unwrap();
        assert!(yuv_bytes(&f) == reference[k * frame_len..(k + 1) * frame_len], "frame {k} differs from ffmpeg");
    }
}

/// VP9 RGB (profile 1, colour space sRGB; planes G, B, R) decodes to RGBA in the right order.
#[test]
fn mkv_vp9_rgb() {
    let Some(b) = fixture(
        "orange_vp9_gbrp.mkv",
        &["-f", "lavfi", "-i", "color=c=0xe08020:s=128x96:r=25:d=0.4", "-c:v", "libvpx-vp9", "-pix_fmt", "gbrp", "-deadline", "realtime", "-lossless", "1"],
    ) else {
        return;
    };
    let src = crate::open_bytes("orange_vp9_gbrp.mkv", b).unwrap();
    let f = src.video_frame(FrameRequest::full(Tick(TICKS_PER_SECOND / 5))).unwrap();
    let px = f.to_rgba8();
    let c = &px[(48 * 128 + 64) * 4..][..3];
    // (the lavfi colour source is converted to RGB by ffmpeg, which rounds by a level)
    for (got, want) in c.iter().zip([0xe0u8, 0x80, 0x20]) {
        assert!((*got as i32 - want as i32).abs() <= 2, "{c:?}");
    }
}
