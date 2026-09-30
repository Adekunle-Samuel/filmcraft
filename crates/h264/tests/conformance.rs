//! Bit-exactness against ffmpeg's decoder on libx264-generated fixtures (skipped when ffmpeg is absent).

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
    intra_cavlc,
    intra_cavlc_noise,
    intra_cavlc_8x8,
    intra_cavlc_cqm,
    p_cavlc_nodeblock,
    baseline_qcif,
    baseline_cif_noise,
    cavlc_b,
    cavlc_b_temporal,
    cavlc_weightp,
    slices4_cavlc,
    qp1_cavlc,
    main_cabac_b,
    high_720p,
    high_1080p,
    crop_1918x1078,
    bpyramid,
    weightp2,
    weightb,
    direct_temporal,
    direct_spatial,
    ref4,
    no_deblock,
    deblock_m2_2,
    cqm_jvt,
    slices4,
    keyint10,
    open_gop,
    constrained_intra,
    no8x8dct,
    qp50,
    qp1,
    vt_high,
    vt_main,
    vt_baseline,
    vt_high_1080p,
);

/// Print which coding tools each fixture exercises (run with `-- --ignored --nocapture`).
#[test]
#[ignore]
fn coverage_report() {
    for f in common::FIXTURES {
        let Some((h264, _)) = common::ensure(f) else { return };
        let data = std::fs::read(h264).unwrap();
        let mut dec = filmcraft_h264::Decoder::new();
        for au in common::split_access_units(&data) {
            dec.decode(au, 0).unwrap();
        }
        dec.flush();
        let s = dec.stats();
        println!(
            "{:<20} pics {:>3} cavlc/cabac {:>3}/{:<3} I/P/B {:>3}/{:>3}/{:>3} i4 {:>6} i8 {:>6} i16 {:>6} pcm {:>4} pskip {:>6} bskip {:>6} bdirect {:>5} inter {:>6} t8 {:>6} mmco {:>3} lt {} gaps {} wp {} tdirect {}",
            f.name,
            s.pictures,
            s.slices_cavlc,
            s.slices_cabac,
            s.slices_i,
            s.slices_p,
            s.slices_b,
            s.mb_i4x4,
            s.mb_i8x8,
            s.mb_i16x16,
            s.mb_pcm,
            s.mb_p_skip,
            s.mb_b_skip,
            s.mb_b_direct16x16,
            s.mb_inter,
            s.mb_inter_8x8_transform,
            s.mmco_ops,
            s.long_term_marks,
            s.frame_num_gaps,
            s.weighted_slices,
            s.temporal_direct_slices
        );
    }
}
