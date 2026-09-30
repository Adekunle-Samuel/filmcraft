//! Test fixture generation (ffmpeg/libx264 as an external oracle) and comparison helpers.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

/// One encoder configuration of the fixture matrix.
pub struct Fixture {
    pub name: &'static str,
    /// lavfi source (without size / rate).
    pub source: &'static str,
    pub width: u32,
    pub height: u32,
    pub frames: u32,
    /// Extra video filter appended after the source (e.g. noise, fade).
    pub filter: &'static str,
    /// Encoder arguments.
    pub args: &'static [&'static str],
}

const NOISE: &str = "noise=alls=12:allf=t+u";

pub const FIXTURES: &[Fixture] = &[
    Fixture {
        name: "intra_cavlc",
        source: "testsrc2",
        width: 176,
        height: 144,
        frames: 3,
        filter: "",
        args: &["-profile:v", "baseline", "-x264-params", "keyint=1:no-deblock=1"],
    },
    Fixture {
        name: "intra_cavlc_noise",
        source: "mandelbrot",
        width: 352,
        height: 288,
        frames: 3,
        filter: NOISE,
        args: &["-profile:v", "baseline", "-x264-params", "keyint=1:no-deblock=1"],
    },
    Fixture {
        name: "intra_cavlc_8x8",
        source: "testsrc2",
        width: 352,
        height: 288,
        frames: 3,
        filter: NOISE,
        args: &["-profile:v", "high", "-x264-params", "keyint=1:no-deblock=1:cabac=0"],
    },
    Fixture {
        name: "intra_cavlc_cqm",
        source: "testsrc2",
        width: 352,
        height: 288,
        frames: 3,
        filter: NOISE,
        args: &["-profile:v", "high", "-x264-params", "keyint=1:no-deblock=1:cabac=0:cqm=jvt"],
    },
    Fixture {
        name: "p_cavlc_nodeblock",
        source: "testsrc2",
        width: 352,
        height: 288,
        frames: 20,
        filter: NOISE,
        args: &["-profile:v", "baseline", "-x264-params", "no-deblock=1:ref=3"],
    },
    Fixture {
        name: "baseline_qcif",
        source: "testsrc2",
        width: 176,
        height: 144,
        frames: 30,
        filter: "",
        args: &["-profile:v", "baseline", "-x264-params", "keyint=15"],
    },
    Fixture { name: "baseline_cif_noise", source: "mandelbrot", width: 352, height: 288, frames: 25, filter: NOISE, args: &["-profile:v", "baseline"] },
    Fixture {
        name: "cavlc_b",
        source: "testsrc2",
        width: 352,
        height: 288,
        frames: 30,
        filter: NOISE,
        args: &["-profile:v", "high", "-x264-params", "cabac=0:bframes=3"],
    },
    Fixture {
        name: "cavlc_b_temporal",
        source: "testsrc2",
        width: 352,
        height: 288,
        frames: 30,
        filter: NOISE,
        args: &["-profile:v", "main", "-x264-params", "cabac=0:bframes=3:direct=temporal:weightb=1"],
    },
    Fixture {
        name: "cavlc_weightp",
        source: "testsrc2",
        width: 352,
        height: 288,
        frames: 30,
        filter: "fade=in:0:25",
        args: &["-profile:v", "main", "-x264-params", "cabac=0:weightp=2:bframes=2:weightb=1"],
    },
    Fixture { name: "main_cabac_b", source: "testsrc2", width: 352, height: 288, frames: 30, filter: NOISE, args: &["-profile:v", "main"] },
    Fixture { name: "high_720p", source: "smptehdbars", width: 1280, height: 720, frames: 10, filter: NOISE, args: &["-profile:v", "high"] },
    Fixture { name: "high_1080p", source: "testsrc2", width: 1920, height: 1080, frames: 6, filter: "", args: &["-profile:v", "high"] },
    Fixture { name: "crop_1918x1078", source: "testsrc2", width: 1918, height: 1078, frames: 5, filter: NOISE, args: &["-profile:v", "high"] },
    Fixture {
        name: "bpyramid",
        source: "testsrc2",
        width: 352,
        height: 288,
        frames: 30,
        filter: NOISE,
        args: &["-profile:v", "high", "-x264-params", "bframes=3:b-pyramid=normal"],
    },
    Fixture {
        name: "weightp2",
        source: "testsrc2",
        width: 352,
        height: 288,
        frames: 30,
        filter: "fade=in:0:25",
        args: &["-profile:v", "high", "-x264-params", "weightp=2"],
    },
    Fixture {
        name: "weightb",
        source: "testsrc2",
        width: 352,
        height: 288,
        frames: 30,
        filter: "fade=in:0:25",
        args: &["-profile:v", "high", "-x264-params", "weightb=1:bframes=3"],
    },
    Fixture {
        name: "direct_temporal",
        source: "testsrc2",
        width: 352,
        height: 288,
        frames: 30,
        filter: NOISE,
        args: &["-profile:v", "high", "-x264-params", "direct=temporal:bframes=3"],
    },
    Fixture {
        name: "direct_spatial",
        source: "mandelbrot",
        width: 352,
        height: 288,
        frames: 30,
        filter: NOISE,
        args: &["-profile:v", "high", "-x264-params", "direct=spatial:bframes=3"],
    },
    Fixture {
        name: "ref4",
        source: "testsrc2",
        width: 352,
        height: 288,
        frames: 30,
        filter: NOISE,
        args: &["-profile:v", "high", "-x264-params", "ref=4:bframes=2"],
    },
    Fixture {
        name: "no_deblock",
        source: "testsrc2",
        width: 352,
        height: 288,
        frames: 20,
        filter: NOISE,
        args: &["-profile:v", "high", "-x264-params", "no-deblock=1"],
    },
    Fixture {
        name: "deblock_m2_2",
        source: "testsrc2",
        width: 352,
        height: 288,
        frames: 20,
        filter: NOISE,
        args: &["-profile:v", "high", "-x264-params", "deblock=-2,2"],
    },
    Fixture {
        name: "cqm_jvt",
        source: "testsrc2",
        width: 352,
        height: 288,
        frames: 20,
        filter: NOISE,
        args: &["-profile:v", "high", "-x264-params", "cqm=jvt"],
    },
    Fixture {
        name: "slices4",
        source: "testsrc2",
        width: 352,
        height: 288,
        frames: 20,
        filter: NOISE,
        args: &["-profile:v", "high", "-x264-params", "slices=4"],
    },
    Fixture {
        name: "slices4_cavlc",
        source: "testsrc2",
        width: 352,
        height: 288,
        frames: 20,
        filter: NOISE,
        args: &["-profile:v", "baseline", "-x264-params", "slices=4"],
    },
    Fixture {
        name: "keyint10",
        source: "testsrc2",
        width: 352,
        height: 288,
        frames: 30,
        filter: NOISE,
        args: &["-profile:v", "high", "-x264-params", "keyint=10"],
    },
    Fixture {
        name: "open_gop",
        source: "testsrc2",
        width: 352,
        height: 288,
        frames: 40,
        filter: NOISE,
        args: &["-profile:v", "high", "-x264-params", "open-gop=1:keyint=12:bframes=3"],
    },
    Fixture {
        name: "constrained_intra",
        source: "testsrc2",
        width: 352,
        height: 288,
        frames: 20,
        filter: NOISE,
        args: &["-profile:v", "high", "-x264-params", "constrained-intra=1"],
    },
    Fixture {
        name: "no8x8dct",
        source: "testsrc2",
        width: 352,
        height: 288,
        frames: 20,
        filter: NOISE,
        args: &["-profile:v", "high", "-x264-params", "8x8dct=0"],
    },
    Fixture { name: "qp50", source: "testsrc2", width: 352, height: 288, frames: 20, filter: NOISE, args: &["-profile:v", "high", "-qp", "50"] },
    Fixture { name: "qp1", source: "mandelbrot", width: 352, height: 288, frames: 10, filter: NOISE, args: &["-profile:v", "high", "-qp", "1"] },
    Fixture {
        name: "qp1_cavlc",
        source: "mandelbrot",
        width: 176,
        height: 144,
        frames: 10,
        filter: NOISE,
        args: &["-profile:v", "main", "-qp", "1", "-x264-params", "cabac=0"],
    },
];

pub fn fixture(name: &str) -> &'static Fixture {
    FIXTURES.iter().find(|f| f.name == name).unwrap_or_else(|| panic!("unknown fixture {name}"))
}

pub fn ffmpeg() -> Option<PathBuf> {
    for p in ["/opt/homebrew/bin/ffmpeg", "/usr/local/bin/ffmpeg", "/usr/bin/ffmpeg"] {
        if Path::new(p).exists() {
            return Some(PathBuf::from(p));
        }
    }
    None
}

pub fn fixtures_dir() -> PathBuf {
    let d = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/fixtures/h264");
    std::fs::create_dir_all(&d).expect("create fixtures dir");
    d
}

fn run(cmd: &mut Command) -> bool {
    match cmd.output() {
        Ok(o) if o.status.success() => true,
        Ok(o) => {
            eprintln!("command failed: {:?}\n{}", cmd, String::from_utf8_lossy(&o.stderr));
            false
        }
        Err(e) => {
            eprintln!("failed to run {:?}: {e}", cmd);
            false
        }
    }
}

/// Generate (if needed) the fixture stream and its ffmpeg reference decode.
/// Returns None (with a message) when ffmpeg is unavailable.
pub fn ensure(f: &Fixture) -> Option<(PathBuf, PathBuf)> {
    let Some(ff) = ffmpeg() else {
        eprintln!("SKIP {}: ffmpeg not found", f.name);
        return None;
    };
    let dir = fixtures_dir();
    let h264 = dir.join(format!("{}.h264", f.name));
    let yuv = dir.join(format!("{}.yuv", f.name));
    if !h264.exists() {
        let tmp = dir.join(format!("{}.tmp.{}.h264", f.name, std::process::id()));
        let mut vf = format!("{}=size={}x{}:rate=25,format=yuv420p", f.source, f.width, f.height);
        if !f.filter.is_empty() {
            vf.push(',');
            vf.push_str(f.filter);
        }
        let mut c = Command::new(&ff);
        c.args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i", &vf]);
        c.args(["-frames:v", &f.frames.to_string(), "-c:v", "libx264"]);
        c.args(f.args);
        c.args(["-f", "h264"]).arg(&tmp);
        assert!(run(&mut c), "fixture generation failed for {}", f.name);
        std::fs::rename(&tmp, &h264).unwrap();
    }
    if !yuv.exists() {
        let tmp = dir.join(format!("{}.tmp.{}.yuv", f.name, std::process::id()));
        let mut c = Command::new(&ff);
        c.args(["-hide_banner", "-loglevel", "error", "-y", "-i"]).arg(&h264);
        c.args(["-fps_mode", "passthrough", "-f", "rawvideo", "-pix_fmt", "yuv420p"]).arg(&tmp);
        assert!(run(&mut c), "reference decode failed for {}", f.name);
        std::fs::rename(&tmp, &yuv).unwrap();
    }
    Some((h264, yuv))
}

/// Split an Annex-B stream into access units (new AU at AUD/SPS/PPS/SEI after a slice, or at a slice with
/// first_mb_in_slice == 0).
pub fn split_access_units(data: &[u8]) -> Vec<&[u8]> {
    // find start code positions
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            let s = if i > 0 && data[i - 1] == 0 { i - 1 } else { i };
            starts.push((s, i + 3));
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut aus = Vec::new();
    let mut au_start = 0usize;
    let mut seen_slice = false;
    for &(sc, payload) in &starts {
        if payload >= data.len() {
            continue;
        }
        let t = data[payload] & 0x1f;
        let is_slice = t == 1 || t == 5;
        let first_mb_zero = is_slice && payload + 1 < data.len() && data[payload + 1] & 0x80 != 0;
        let boundary = seen_slice && (matches!(t, 6..=9) || first_mb_zero);
        if boundary {
            aus.push(&data[au_start..sc]);
            au_start = sc;
            seen_slice = false;
        }
        if is_slice {
            seen_slice = true;
        }
    }
    if au_start < data.len() {
        aus.push(&data[au_start..]);
    }
    aus
}
