//! Inter-frame bit-exactness against libdav1d (run as an external ffmpeg process): SVT-AV1
//! GOPs with hierarchical references, compound prediction, OBMC / warped motion, global motion,
//! motion-field projection and all loop filters.

mod common;
use common::*;

fn specs() -> Vec<Spec> {
    let p = |preset: &'static str, crf: &'static str| -> Vec<&'static str> { vec!["-preset", preset, "-crf", crf] };
    vec![
        Spec::new("inter_testsrc_p8", "testsrc2=s=320x240:r=25", 10, "yuv420p", &p("8", "35")),
        Spec::new("inter_testsrc_p3", "testsrc2=s=352x288:r=25", 24, "yuv420p", &p("3", "30")),
        Spec::new("inter_mandel_10bit_p5", "mandelbrot=s=416x240:r=25", 16, "yuv420p10le", &p("5", "40")),
        Spec::new("inter_noise_odd_p6", "testsrc2=s=203x117:r=25,noise=alls=15:allf=t", 12, "yuv420p", &p("6", "45")),
    ]
}

#[test]
fn inter_gops() {
    let Some(ff) = ffmpeg() else { return };
    for s in specs() {
        check_bit_exact(&ff, &s);
    }
}
