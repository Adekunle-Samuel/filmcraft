//! Bit-exactness against ffmpeg's VP9 decoder on libvpx-vp9 fixtures (skipped when ffmpeg or
//! libvpx is absent).

mod common;

macro_rules! fixture_tests {
    ($($name:ident),* $(,)?) => {
        $(
            #[test]
            fn $name() {
                match common::check_fixture(stringify!($name)) {
                    Ok(true) => {}
                    Ok(false) => eprintln!("skipped {}", stringify!($name)),
                    Err(e) => panic!("{e}"),
                }
            }
        )*
    };
}

fixture_tests!(
    intra_only_keyframes,
    default_good,
    profile2_10bit,
    profile2_12bit,
    profile1_444,
    profile1_422,
    profile1_440,
    profile3_444_10bit,
    lossless,
    lossless_10bit,
    tiles_cols4,
    tiles_rows_cols,
    altref_hidden,
    frame_parallel,
    frame_parallel_off,
    error_resilient,
    odd_size,
    tiny_odd,
    row_mt,
    speed0,
    speed2,
    speed5,
    speed8,
    cq_low,
    cq_high,
    cq_10bit_low,
    aq_segmentation,
    aq_variance,
    sharpness,
    fade_intra_heavy,
    hd_1080p,
);

/// Resolution changes mid-stream: libvpx's `resize_mode` (dynamic internal scaling with scaled
/// reference prediction), and concatenated streams of different sizes.
#[test]
fn resize_dynamic() {
    let Some(ff) = common::ffmpeg() else { return };
    let dir = common::fixtures_dir();
    let ivf = dir.join("resize_dynamic.ivf");
    let yuv = dir.join("resize_dynamic.yuv");
    if !ivf.exists() {
        // Encode with a spatial resampling switch: first 10 frames at 352x288, then the encoder
        // is fed 176x144 content via a scale filter that changes size (ffmpeg reinitialises
        // libvpx with a new size without a key frame when `-resize_mode` style scaling is
        // unavailable, so emulate with libvpx's internal resize through the `resize-mode` knob
        // of vpxenc-like options if present, otherwise concatenation below covers size changes).
        let mut c = std::process::Command::new(&ff);
        c.args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i", "testsrc2=size=352x288:rate=25,noise=alls=10:allf=t+u"]);
        c.args([
            "-frames:v",
            "30",
            "-c:v",
            "libvpx-vp9",
            "-b:v",
            "60k",
            "-minrate",
            "60k",
            "-maxrate",
            "60k",
            "-undershoot-pct",
            "0",
            "-lag-in-frames",
            "0",
            "-deadline",
            "realtime",
            "-speed",
            "8",
        ]);
        c.args(["-vpx-options", "resize-mode=3"]);
        c.args(["-f", "ivf"]).arg(&ivf);
        if !common::run(&mut c) {
            let mut c = std::process::Command::new(&ff);
            c.args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i", "testsrc2=size=352x288:rate=25,noise=alls=10:allf=t+u"]);
            c.args(["-frames:v", "30", "-c:v", "libvpx-vp9", "-b:v", "40k", "-deadline", "realtime", "-speed", "8", "-lag-in-frames", "0"]);
            c.args(["-f", "ivf"]).arg(&ivf);
            assert!(common::run(&mut c));
        }
    }
    if !yuv.exists() {
        assert!(common::reference_decode(&ivf, &yuv, "yuv420p"));
    }
    let reference = std::fs::read(&yuv).unwrap();
    common::check_files("resize_dynamic", &ivf, &reference, "yuv420p").unwrap();
}

/// Two streams of different sizes concatenated into one IVF (key frame at the size change).
#[test]
fn resize_concatenated() {
    let a = common::fixture("default_good");
    let b = common::fixture("odd_size");
    let (Some((ia, ya)), Some((ib, yb))) = (common::ensure(a), common::ensure(b)) else { return };
    let da = std::fs::read(ia).unwrap();
    let db = std::fs::read(ib).unwrap();
    let mut frames: Vec<Vec<u8>> = common::ivf_frames(&da).into_iter().map(|f| f.to_vec()).collect();
    frames.extend(common::ivf_frames(&db).into_iter().map(|f| f.to_vec()));
    let path = common::fixtures_dir().join("resize_concat.ivf");
    common::write_ivf(&path, 352, 288, &frames);
    let mut reference = std::fs::read(ya).unwrap();
    reference.extend(std::fs::read(yb).unwrap());
    common::check_files("resize_concat", &path, &reference, "yuv420p").unwrap();
}

/// Print which coding tools each fixture exercises (run with `-- --ignored --nocapture`).
#[test]
#[ignore]
fn coverage_report() {
    for f in common::FIXTURES {
        let Some((ivf, _)) = common::ensure(f) else { continue };
        let data = std::fs::read(ivf).unwrap();
        let mut dec = filmcraft_vp9::Decoder::new();
        for fr in common::ivf_frames(&data) {
            let _ = dec.decode(fr, 0);
        }
        println!("{:<22} {:?}", f.name, dec.stats());
    }
}

/// The comparison must report a single flipped sample (guards against a vacuous harness).
#[test]
fn compare_detects_mismatch() {
    let f = common::fixture("intra_only_keyframes");
    let Some((ivf, yuv)) = common::ensure(f) else { return };
    let pics = common::decode_file_threads(&ivf, 1).map_err(|e| e.1).unwrap();
    let mut reference = std::fs::read(yuv).unwrap();
    common::compare(&pics, &reference, f.pix_fmt).unwrap();
    let pos = reference.len() - 7;
    reference[pos] ^= 1;
    let err = common::compare(&pics, &reference, f.pix_fmt).unwrap_err();
    assert!(err.contains("plane V"), "{err}");
}
