# filmcraft-av1

Clean-room, pure-Rust (no `unsafe`) AV1 decoder, implemented from the public **AV1 Bitstream &
Decoding Process Specification, version 1.0.0 with Errata 1** (AOMediaCodec/av1-spec, git
commit `5e04f3f`, 2023-06-12). It is an L0 crate: it depends only on `filmcraft-bitstream`,
`thiserror` and (optional, feature `threads`) `rayon`, and builds for `wasm32-unknown-unknown`.

All constant tables (default CDFs, scan orders, quantiser lookups, filter taps, …) and the
named constants are extracted from the specification's Markdown source by
[`tools/extract_tables.py`](tools/extract_tables.py) into `src/spec_tables.rs` (every table's
shape is checked against its declared dimensions; one missing comma in the spec's
`Split_Tx_Size` initialiser is reported and repaired). No code was taken from libaom, dav1d,
SVT-AV1 or any other implementation; ffmpeg (libsvtav1 to encode fixtures, libdav1d to decode the
reference) is used only as an external test oracle.

## Conformance status

| Stage | Spec | Status |
|---|---|---|
| OBU parsing (low-overhead format), temporal delimiters, operating-point dropping | 5.3, 7.5 | done |
| Sequence header, color config, timing / decoder model info | 5.5 | done |
| Uncompressed frame header (all fields, frame size / superres / render size, tile info, quantiser, segmentation, delta q/lf, loop filter, CDEF, LR, tx mode, skip mode, global motion, film grain params) | 5.9 | done |
| Reference frame state: set_frame_refs, setup_past_independence, load_previous, reference update, show_existing_frame | 7.8, 7.20, 7.21 | done |
| Symbol decoder, CDF adaptation, init / load / save / frame-end CDF update | 8.2, 8.3 | done |
| Tiles and tile groups (uniform and explicit spacing, tile size bytes) | 5.11 | done (decoded sequentially) |
| Intra frame mode info: partitions, skip, segment id, CDEF index, delta q / lf, y / uv modes, angle deltas, CfL alphas, palette (colors, cache, color index map), filter intra, tx size (incl. var-tx syntax) | 5.11 | done, bit-exact |
| Coefficients (all_zero, eob, base / br levels, Golomb, dc sign, tx type sets, scans) | 5.11.39 | done, bit-exact |
| Dequantisation incl. quantiser matrices | 7.12 | done (qmatrix not yet oracle-tested) |
| Inverse transforms: DCT 4–64, ADST 4/8/16, flip ADST, identity 4–32, WHT (lossless), rectangular | 7.13 | done, bit-exact |
| Intra prediction: DC, V/H + directional with edge filter and upsampling, smooth (3), Paeth, recursive filter intra, CfL, palette | 7.11.2, 7.11.4, 7.11.5 | done, bit-exact |
| Inter mode info: segment id prediction, skip mode, reference frames (single / compound, uni / bi), modes, DRL, MV coding, inter-intra, motion mode, compound type, interpolation filters | 5.11.18–5.11.32 | done, bit-exact |
| MV prediction: spatial / temporal candidate stacks, global MV, extra search, clamping, warp samples | 7.10 | done, bit-exact |
| Motion field estimation (projection) and motion vector storage | 7.9, 7.19 | done, bit-exact |
| Inter prediction: MV scaling (scaled references), 8-tap sub-pixel filters, global and local warp (warp estimation, shear), OBMC, wedge / difference-weighted / inter-intra masks, distance weights, averaging | 7.11.3 | done, bit-exact |
| Intra block copy | 7.11.3 | done, bit-exact |
| Output policy with scalability (highest spatial layer per temporal unit) | 7.18.1 | done, bit-exact |
| Loop filter (all filter sizes, deltas, segment / ref / mode adjustments) | 7.14 | done, bit-exact |
| CDEF | 7.15 | done, bit-exact |
| Super-resolution upscaling | 7.16 | done, not yet oracle-tested (no fixture uses it yet) |
| Loop restoration (Wiener, self-guided, switchable; stripes) | 7.17 | done, bit-exact |
| Film grain synthesis | 7.18.3 | missing |
| Large-scale tile / tile list OBUs | 7.3 | not planned |

## Accuracy

`tests/oracle_intra.rs` encodes all-intra streams with ffmpeg's libsvtav1 (with and without the
in-loop filters; 64×64 and 128×128 superblocks; intra edge filter on and off) and compares every
decoded frame with libdav1d: **bit-exact** on all 13 fixtures (8- and 10-bit 4:2:0; 128×128 to
1280×720 including odd sizes; testsrc2, mandelbrot, gradients and noise; presets 1–8).

`tests/conformance.rs` downloads libaom's conformance test vectors on first use
(storage.googleapis.com/aom-test-data) and compares with libdav1d: `av1-1-b8-02-allintra`
(39 frames, deblocking + CDEF + self-guided restoration), `05-mv`, `06-mfmv` and
`24-monochrome` are **bit-exact** in the default run. The ignored
`conformance_vectors_extended` test covers the wider set; bit-exact today: `01-size-16x16`,
`-66x66`, `-196x196`, `-226x226`, `00-quantizer-00/31/63` (8-bit) and `-00/40` (10-bit),
`04-cdfupdate`, `05-mv`, `06-mfmv`, `22-svc-L1T2`, `22-svc-L2T1`, `22-svc-L2T2` (spatial layers
with scaled inter-layer prediction), `24-monochrome` (8 and 10-bit) and
`16-intra_only-intrabc-extreme-dv` (1080p intra block copy).

`tests/oracle_inter.rs` encodes SVT-AV1 GOPs (hierarchical references, compound prediction,
OBMC / warped motion, motion-field projection, all loop filters; presets 3–8, 8- and 10-bit, odd
sizes) and compares every frame with libdav1d: **bit-exact** on all 4 fixtures.

## API

```rust
let mut dec = filmcraft_av1::Decoder::new();
for sample in samples {                  // one temporal unit (MP4 / Matroska sample) each
    for pic in dec.decode(&sample)? {    // shown frames, planar u16
        // pic.width, pic.height, pic.bit_depth, pic.planes[0..3]
    }
}
```
