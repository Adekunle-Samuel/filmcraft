//! Shared helpers for the AV1 oracle tests. ffmpeg (libsvtav1 to encode fixtures, libdav1d to
//! decode the reference) is used only as an external process.
#![allow(dead_code)]

use filmcraft_av1::{Decoder, Picture};
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn ffmpeg() -> Option<PathBuf> {
    filmcraft_testkit::ffmpeg_or_skip("av1 oracle")
}

pub fn fixture_dir() -> PathBuf {
    filmcraft_testkit::fixtures_dir("av1")
}

pub fn run(ff: &Path, args: &[&str]) {
    let out = Command::new(ff).args(["-hide_banner", "-loglevel", "error", "-y"]).args(args).output().expect("run ffmpeg");
    assert!(out.status.success(), "ffmpeg {:?} failed: {}", args, String::from_utf8_lossy(&out.stderr));
}

/// An AV1 fixture: a lavfi source encoded by ffmpeg's libsvtav1 into IVF.
pub struct Spec {
    pub name: &'static str,
    pub lavfi: String,
    pub frames: u32,
    pub pix_fmt: &'static str,
    /// Extra encoder arguments.
    pub enc: Vec<String>,
}

impl Spec {
    pub fn new(name: &'static str, lavfi: impl Into<String>, frames: u32, pix_fmt: &'static str, enc: &[&str]) -> Spec {
        Spec { name, lavfi: lavfi.into(), frames, pix_fmt, enc: enc.iter().map(|s| s.to_string()).collect() }
    }
}

pub fn make(ff: &Path, spec: &Spec) -> PathBuf {
    let path = fixture_dir().join(format!("{}.ivf", spec.name));
    if !path.exists() {
        let tmp = filmcraft_testkit::temp_path(&path);
        let frames = spec.frames.to_string();
        let t = tmp.to_str().unwrap().to_string();
        let mut args: Vec<&str> = vec!["-f", "lavfi", "-i", spec.lavfi.as_str(), "-frames:v", frames.as_str(), "-pix_fmt", spec.pix_fmt, "-c:v", "libsvtav1"];
        args.extend(spec.enc.iter().map(String::as_str));
        args.extend(["-f", "ivf", t.as_str()]);
        run(ff, &args);
        std::fs::rename(&tmp, &path).unwrap();
    }
    path
}

/// Frames of an IVF file.
pub fn ivf_frames(path: &Path) -> Vec<Vec<u8>> {
    let d = std::fs::read(path).unwrap();
    assert_eq!(&d[0..4], b"DKIF");
    let hdr = u16::from_le_bytes([d[6], d[7]]) as usize;
    let mut pos = hdr;
    let mut out = Vec::new();
    while pos + 12 <= d.len() {
        let n = u32::from_le_bytes([d[pos], d[pos + 1], d[pos + 2], d[pos + 3]]) as usize;
        pos += 12;
        out.push(d[pos..pos + n].to_vec());
        pos += n;
    }
    out
}

/// Reference decode with ffmpeg's libdav1d (8-bit formats widened to u16).
pub fn reference(ff: &Path, path: &Path, pix_fmt: &str) -> Vec<u16> {
    let out = Command::new(ff)
        .args(["-hide_banner", "-loglevel", "error", "-c:v", "libdav1d", "-i"])
        .arg(path)
        .args(["-f", "rawvideo", "-pix_fmt", pix_fmt, "-"])
        .output()
        .expect("run ffmpeg");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    if pix_fmt.ends_with("le") {
        out.stdout.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect()
    } else {
        out.stdout.iter().map(|&b| b as u16).collect()
    }
}

/// Decode every frame of an IVF file with our decoder.
pub fn decode_all(path: &Path) -> Result<Vec<Picture>, String> {
    let mut dec = Decoder::new();
    let mut pics = Vec::new();
    for (i, f) in ivf_frames(path).iter().enumerate() {
        let p = dec.decode(f).map_err(|e| format!("frame {i}: {e}"))?;
        pics.extend(p);
    }
    Ok(pics)
}

pub fn picture_samples(p: &Picture) -> usize {
    let mut n = (p.width * p.height) as usize;
    if !p.mono_chrome {
        n += 2 * (p.plane_width(1) * p.plane_height(1)) as usize;
    }
    n
}

/// Compare a decoded picture with the raw reference; returns the first mismatch as
/// (plane, x, y, ours, reference) and the mismatch count.
pub fn compare(p: &Picture, raw: &[u16]) -> (Option<(usize, u32, u32, u16, u16)>, usize) {
    let mut first = None;
    let mut count = 0;
    let mut off = 0;
    for plane in 0..if p.mono_chrome { 1 } else { 3 } {
        let w = p.plane_width(plane);
        let h = p.plane_height(plane);
        for y in 0..h {
            for x in 0..w {
                let a = p.planes[plane][(y * w + x) as usize];
                let b = raw[off + (y * w + x) as usize];
                if a != b {
                    count += 1;
                    if first.is_none() {
                        first = Some((plane, x, y, a, b));
                    }
                }
            }
        }
        off += (w * h) as usize;
    }
    (first, count)
}

/// Decode a fixture and compare every frame with libdav1d; panics with details on mismatch.
pub fn check_bit_exact(ff: &Path, spec: &Spec) {
    let path = make(ff, spec);
    let pics = decode_all(&path).unwrap_or_else(|e| panic!("{}: {e}", spec.name));
    let raw = reference(ff, &path, spec.pix_fmt);
    assert!(!pics.is_empty(), "{}: no frames", spec.name);
    let per = picture_samples(&pics[0]);
    assert_eq!(raw.len(), per * pics.len(), "{}: frame count/size ({} pictures)", spec.name, pics.len());
    for (i, p) in pics.iter().enumerate() {
        let (first, count) = compare(p, &raw[i * per..(i + 1) * per]);
        if let Some((plane, x, y, a, b)) = first {
            panic!("{}: frame {i} differs in {count} samples; first at plane {plane} ({x},{y}): ours {a}, reference {b}", spec.name);
        }
    }
    println!("{:<28} {} frames {}x{} bit-exact", spec.name, pics.len(), pics[0].width, pics[0].height);
}
