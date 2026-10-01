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
    profile3_422_12bit,
    lossless,
    lossless_10bit,
    tiles_cols4,
    tiles_rows_cols,
    tiles_rows4,
    altref_hidden,
    altref_10bit,
    altref_tiles,
    frame_parallel,
    error_resilient,
    odd_size,
    tiny_odd,
    row_mt,
    speed0,
    speed1,
    speed2,
    speed3,
    speed4,
    speed5,
    speed6,
    speed7,
    speed8,
    cq_low,
    cq_q4,
    cq_high,
    cq_10bit_low,
    cq_12bit_high,
    aq_segmentation,
    aq_variance,
    aq_complexity,
    sharpness,
    fade_intra_heavy,
    hd_1080p,
);

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
