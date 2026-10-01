//! Decode throughput (ignored; run with `cargo test --release -p filmcraft-av1 --test perf --
//! --ignored --nocapture`).

mod common;
use common::*;
use std::time::Instant;

fn bench(spec: &Spec) {
    let Some(ff) = ffmpeg() else { return };
    let path = make(&ff, spec);
    let frames = ivf_frames(&path);
    let mut best = f64::INFINITY;
    let mut shown = 0;
    for _ in 0..3 {
        let mut dec = filmcraft_av1::Decoder::new();
        let t = Instant::now();
        shown = 0;
        for f in &frames {
            shown += dec.decode(f).unwrap().len();
        }
        best = best.min(t.elapsed().as_secs_f64());
    }
    println!("{:<28} {:>3} frames  {:>6.1} fps", spec.name, shown, shown as f64 / best);
}

#[test]
#[ignore]
fn decode_throughput() {
    let p = |crf: &'static str, params: &'static str| -> Vec<&'static str> { vec!["-preset", "8", "-crf", crf, "-svtav1-params", params] };
    bench(&Spec::new("perf_1080p_intra", "testsrc2=s=1920x1080:r=25,noise=alls=6:allf=t", 10, "yuv420p", &p("30", "keyint=1")));
    bench(&Spec::new("perf_1080p_gop", "testsrc2=s=1920x1080:r=25,noise=alls=6:allf=t", 60, "yuv420p", &p("30", "keyint=60")));
    bench(&Spec::new("perf_1080p_gop_10bit", "testsrc2=s=1920x1080:r=25,noise=alls=6:allf=t", 60, "yuv420p10le", &p("30", "keyint=60")));
}
