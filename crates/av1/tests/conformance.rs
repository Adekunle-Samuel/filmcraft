//! libaom conformance test vectors (downloaded on first use), compared with libdav1d.

mod common;
use common::*;

/// Vectors the decoder handles today (more are added as stages land).
const VECTORS: &[&str] = &["av1-1-b8-02-allintra.ivf"];

#[test]
fn conformance_vectors() {
    let Some(ff) = ffmpeg() else { return };
    for name in VECTORS {
        let Some(path) = test_vector(name) else { return };
        check_file(&ff, name, &path);
    }
}
