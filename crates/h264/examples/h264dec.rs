//! Decode an Annex-B H.264 file to raw yuv420p: `h264dec in.h264 [out.yuv]`.
//! Prints timing information to stderr.

use filmcraft_h264::Decoder;
use std::io::Write;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: h264dec in.h264 [out.yuv]");
        std::process::exit(2);
    }
    let data = std::fs::read(&args[1]).expect("read input");
    let mut out = args.get(2).map(|p| std::io::BufWriter::new(std::fs::File::create(p).expect("create output")));
    let mut dec = Decoder::new();
    let t0 = std::time::Instant::now();
    let mut n = 0usize;
    let mut write = |pics: Vec<filmcraft_h264::Picture>| {
        for p in pics {
            n += 1;
            if let Some(o) = out.as_mut() {
                o.write_all(&p.y).unwrap();
                o.write_all(&p.u).unwrap();
                o.write_all(&p.v).unwrap();
            }
        }
    };
    match dec.decode(&data, 0) {
        Ok(p) => write(p),
        Err(e) => eprintln!("decode error: {e}"),
    }
    write(dec.flush());
    let dt = t0.elapsed().as_secs_f64();
    eprintln!("{n} frames in {dt:.3}s ({:.1} fps)", n as f64 / dt);
}
