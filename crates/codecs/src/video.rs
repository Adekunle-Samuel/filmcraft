//! The video decoder trait and the built-in Motion-JPEG decoder.

use filmcraft_frame::VideoFrame;
use filmcraft_isobmff::{CodecConfig, SampleEntry};

use crate::{CodecError, Result};

/// A decoded picture with its presentation timestamp (track timescale).
pub struct DecodedFrame {
    pub pts: i64,
    pub frame: VideoFrame,
}

/// A stateful video decoder. Samples are fed in decode order; pictures come out in
/// presentation order (a decoder with reordering may return zero or several per call).
pub trait VideoDecoder: Send {
    fn decode(&mut self, sample: &[u8], pts: i64) -> Result<Vec<DecodedFrame>>;
    /// Drain pictures held for reordering (end of stream / before a seek).
    fn flush(&mut self) -> Vec<DecodedFrame>;
    /// Forget all state (called after seeking to a sync sample).
    fn reset(&mut self);
    fn name(&self) -> &str;
    /// Every picture is coded independently (no reordering, no references), so frames can be
    /// decoded in any order and in parallel by separate decoder instances.
    fn intra_only(&self) -> bool {
        false
    }
    /// Whether decoding can start at `sample` (`None`: unknown, trust the container's sync flags).
    /// Containers may flag samples as sync that the codec cannot start from (an MP4 without
    /// `stss` marks every sample), so codecs that can tell say so.
    fn is_random_access(&self, _sample: &[u8]) -> Option<bool> {
        None
    }
}

/// A factory returns `None` when it does not handle the entry.
pub type VideoDecoderFactory = fn(&SampleEntry) -> Option<Result<Box<dyn VideoDecoder>>>;

/// Motion-JPEG / Photo-JPEG (each sample is a complete JPEG).
pub struct MjpegDecoder;

impl VideoDecoder for MjpegDecoder {
    fn decode(&mut self, sample: &[u8], pts: i64) -> Result<Vec<DecodedFrame>> {
        // mjpa samples may contain two fields; decode the first image (field-merging lands with interlace support).
        let img = image::load_from_memory_with_format(sample, image::ImageFormat::Jpeg).map_err(|e| CodecError::Decode(e.to_string()))?;
        let rgba = img.to_rgba8();
        let (w, h) = rgba.dimensions();
        Ok(vec![DecodedFrame { pts, frame: VideoFrame::rgba8(w, h, rgba.into_raw()) }])
    }
    fn flush(&mut self) -> Vec<DecodedFrame> {
        Vec::new()
    }
    fn reset(&mut self) {}
    fn name(&self) -> &str {
        "Motion JPEG"
    }
    fn intra_only(&self) -> bool {
        true
    }
}

pub fn mjpeg_factory(e: &SampleEntry) -> Option<Result<Box<dyn VideoDecoder>>> {
    matches!(e.codec, CodecConfig::Jpeg { .. }).then(|| Ok(Box::new(MjpegDecoder) as Box<dyn VideoDecoder>))
}

/// Our pure-Rust H.264 decoder (frame-threaded).
pub struct H264Decoder {
    avcc: Vec<u8>,
    dec: filmcraft_h264::Decoder,
}

impl H264Decoder {
    pub fn new(avcc: Vec<u8>) -> Result<Self> {
        let dec = filmcraft_h264::Decoder::from_avcc(&avcc).map_err(|e| CodecError::Decode(e.to_string()))?;
        Ok(Self { avcc, dec })
    }
    fn convert(p: filmcraft_h264::Picture) -> DecodedFrame {
        use std::sync::Arc;
        let (w, h) = (p.width as usize, p.height as usize);
        let (cw, ch) = (p.chroma_width as usize, p.chroma_height as usize);
        let tight = |src: &[u8], stride: usize, w: usize, h: usize| -> Vec<u8> {
            if stride == w && src.len() >= w * h {
                return src[..w * h].to_vec();
            }
            let mut out = Vec::with_capacity(w * h);
            for y in 0..h {
                out.extend_from_slice(&src[y * stride..y * stride + w]);
            }
            out
        };
        let y = tight(&p.y, p.y_stride, w, h);
        let u = tight(&p.u, p.uv_stride, cw, ch);
        let v = tight(&p.v, p.uv_stride, cw, ch);
        let mut color = filmcraft_color::ColorInfo { matrix: filmcraft_frame::default_matrix(p.width, p.height), ..filmcraft_color::ColorInfo::REC709 };
        if let Some(m) = filmcraft_color::Matrix::from_code(p.color.matrix) {
            color.matrix = m;
        }
        if let Some(t) = filmcraft_color::Transfer::from_code(p.color.transfer) {
            color.transfer = t;
        }
        if p.color.full_range {
            color.range = filmcraft_color::Range::Full;
        }
        let par = if p.sar.0 > 0 && p.sar.1 > 0 { (p.sar.0 as u32, p.sar.1 as u32) } else { (1, 1) };
        let frame = VideoFrame {
            width: p.width,
            height: p.height,
            data: filmcraft_frame::PixelData::Yuv8 { planes: [Arc::new(y), Arc::new(u), Arc::new(v)], chroma: filmcraft_frame::Chroma::C420, alpha: None },
            color,
            par,
            pts: filmcraft_time::Tick::ZERO,
        };
        DecodedFrame { pts: p.pts, frame }
    }
}

impl VideoDecoder for H264Decoder {
    fn decode(&mut self, sample: &[u8], pts: i64) -> Result<Vec<DecodedFrame>> {
        let pics = self.dec.decode(sample, pts).map_err(|e| CodecError::Decode(e.to_string()))?;
        Ok(pics.into_iter().map(Self::convert).collect())
    }
    fn flush(&mut self) -> Vec<DecodedFrame> {
        self.dec.flush().into_iter().map(Self::convert).collect()
    }
    fn reset(&mut self) {
        if let Ok(d) = filmcraft_h264::Decoder::from_avcc(&self.avcc) {
            self.dec = d;
        }
    }
    fn name(&self) -> &str {
        "FilmCraft H.264"
    }
}

pub fn h264_factory(e: &SampleEntry) -> Option<Result<Box<dyn VideoDecoder>>> {
    match &e.codec {
        CodecConfig::Avc(a) => Some(H264Decoder::new(a.to_bytes()).map(|d| Box::new(d) as Box<dyn VideoDecoder>)),
        _ => None,
    }
}

/// Our pure-Rust HEVC decoder (Main / Main 10, frame-threaded).
pub struct HevcDecoder {
    hvcc: Vec<u8>,
    dec: filmcraft_hevc::Decoder,
}

impl HevcDecoder {
    pub fn new(hvcc: Vec<u8>) -> Result<Self> {
        let dec = filmcraft_hevc::Decoder::from_hvcc(&hvcc).map_err(|e| CodecError::Decode(e.to_string()))?;
        Ok(Self { hvcc, dec })
    }
    fn convert(p: filmcraft_hevc::Picture) -> DecodedFrame {
        use filmcraft_hevc::Plane;
        use std::sync::Arc;
        fn tight<T: Copy>(src: &[T], stride: usize, w: usize, h: usize) -> Vec<T> {
            if stride == w && src.len() >= w * h {
                return src[..w * h].to_vec();
            }
            let mut out = Vec::with_capacity(w * h);
            for y in 0..h {
                out.extend_from_slice(&src[y * stride..y * stride + w]);
            }
            out
        }
        let (w, h) = (p.width as usize, p.height as usize);
        let (cw, ch) = (p.chroma_width as usize, p.chroma_height as usize);
        let data = match (&p.y, &p.u, &p.v) {
            (Plane::U8(y), Plane::U8(u), Plane::U8(v)) => filmcraft_frame::PixelData::Yuv8 {
                planes: [Arc::new(tight(y, p.y_stride, w, h)), Arc::new(tight(u, p.uv_stride, cw, ch)), Arc::new(tight(v, p.uv_stride, cw, ch))],
                chroma: filmcraft_frame::Chroma::C420,
                alpha: None,
            },
            _ => {
                let wide = |pl: &Plane| -> Vec<u16> { (0..pl.len()).map(|i| pl.get(i)).collect() };
                filmcraft_frame::PixelData::Yuv16 {
                    planes: [
                        Arc::new(tight(&wide(&p.y), p.y_stride, w, h)),
                        Arc::new(tight(&wide(&p.u), p.uv_stride, cw, ch)),
                        Arc::new(tight(&wide(&p.v), p.uv_stride, cw, ch)),
                    ],
                    chroma: filmcraft_frame::Chroma::C420,
                    bits: p.bit_depth,
                    alpha: None,
                }
            }
        };
        let mut color = filmcraft_color::ColorInfo { matrix: filmcraft_frame::default_matrix(p.width, p.height), ..filmcraft_color::ColorInfo::REC709 };
        if let Some(m) = filmcraft_color::Matrix::from_code(p.color.matrix) {
            color.matrix = m;
        }
        if let Some(t) = filmcraft_color::Transfer::from_code(p.color.transfer) {
            color.transfer = t;
        }
        if p.color.full_range {
            color.range = filmcraft_color::Range::Full;
        }
        let par = if p.sar.0 > 0 && p.sar.1 > 0 { (p.sar.0 as u32, p.sar.1 as u32) } else { (1, 1) };
        let frame = VideoFrame { width: p.width, height: p.height, data, color, par, pts: filmcraft_time::Tick::ZERO };
        DecodedFrame { pts: p.pts, frame }
    }
}

impl VideoDecoder for HevcDecoder {
    fn decode(&mut self, sample: &[u8], pts: i64) -> Result<Vec<DecodedFrame>> {
        let pics = self.dec.decode(sample, pts).map_err(|e| CodecError::Decode(e.to_string()))?;
        Ok(pics.into_iter().map(Self::convert).collect())
    }
    fn flush(&mut self) -> Vec<DecodedFrame> {
        self.dec.flush().into_iter().map(Self::convert).collect()
    }
    fn reset(&mut self) {
        if let Ok(d) = filmcraft_hevc::Decoder::from_hvcc(&self.hvcc) {
            self.dec = d;
        }
    }
    fn name(&self) -> &str {
        "FilmCraft HEVC"
    }
}

pub fn hevc_factory(e: &SampleEntry) -> Option<Result<Box<dyn VideoDecoder>>> {
    match &e.codec {
        CodecConfig::Hevc(c) => Some(HevcDecoder::new(c.to_bytes()).map(|d| Box::new(d) as Box<dyn VideoDecoder>)),
        _ => None,
    }
}

/// Our pure-Rust VP9 decoder (profiles 0-3, 8/10/12-bit; tile columns and loop filter decode in
/// parallel).
pub struct Vp9Decoder {
    dec: filmcraft_vp9::Decoder,
    /// Container colour (vpcC / Matroska `Colour`): transfer and primaries are not in the VP9
    /// bitstream.
    transfer: Option<filmcraft_color::Transfer>,
    primaries: Option<filmcraft_color::Primaries>,
}

/// Colour primaries from an ISO/IEC 23091-2 code.
pub(crate) fn primaries_from_code(c: u8) -> Option<filmcraft_color::Primaries> {
    use filmcraft_color::Primaries;
    match c {
        1 => Some(Primaries::Bt709),
        5 => Some(Primaries::Bt601_625),
        6 => Some(Primaries::Bt601_525),
        9 => Some(Primaries::Bt2020),
        12 => Some(Primaries::P3D65),
        _ => None,
    }
}

impl Vp9Decoder {
    pub fn new(cfg: Option<&filmcraft_isobmff::VpcConfig>) -> Self {
        Self {
            dec: filmcraft_vp9::Decoder::new(),
            transfer: cfg.and_then(|c| filmcraft_color::Transfer::from_code(c.transfer_characteristics)),
            primaries: cfg.and_then(|c| primaries_from_code(c.colour_primaries)),
        }
    }

    fn convert(&self, p: filmcraft_vp9::Picture) -> DecodedFrame {
        use filmcraft_color::{Matrix, Primaries, Range};
        use filmcraft_frame::{Chroma, PixelData};
        use filmcraft_vp9::Plane;
        use std::sync::Arc;
        let (w, h) = (p.width as usize, p.height as usize);
        let (cw, ch) = (p.chroma_width as usize, p.chroma_height as usize);
        let mut color = filmcraft_color::ColorInfo { matrix: filmcraft_frame::default_matrix(p.width, p.height), ..filmcraft_color::ColorInfo::REC709 };
        // color_space (7.2.2): 1 BT.601, 2 BT.709, 3 SMPTE-170, 4 SMPTE-240, 5 BT.2020, 7 sRGB.
        match p.color.color_space {
            1 | 3 => color.matrix = Matrix::Bt601,
            2 | 4 => color.matrix = Matrix::Bt709,
            5 => {
                color.matrix = Matrix::Bt2020Ncl;
                color.primaries = Primaries::Bt2020;
            }
            _ => {}
        }
        if let Some(t) = self.transfer {
            color.transfer = t;
        }
        if let Some(pr) = self.primaries {
            color.primaries = pr;
        }
        if p.color.full_range {
            color.range = Range::Full;
        }
        let pts = p.pts;
        if p.color.color_space == 7 {
            // RGB (profiles 1 / 3, 4:4:4): the planes carry G, B, R.
            let shift = p.bit_depth - 8;
            let mut rgba = Vec::with_capacity(w * h * 4);
            for i in 0..w * h {
                rgba.extend_from_slice(&[(p.v.get(i) >> shift) as u8, (p.y.get(i) >> shift) as u8, (p.u.get(i) >> shift) as u8, 255]);
            }
            let mut frame = VideoFrame::rgba8(p.width, p.height, rgba);
            frame.color = filmcraft_color::ColorInfo { range: Range::Full, transfer: filmcraft_color::Transfer::Srgb, ..color };
            return DecodedFrame { pts, frame };
        }
        // 4:4:0 has no frame format of its own: chroma rows are repeated to 4:4:4.
        let (chroma, rows_440) = match (p.subsampling_x, p.subsampling_y) {
            (true, true) => (Chroma::C420, false),
            (true, false) => (Chroma::C422, false),
            (false, false) => (Chroma::C444, false),
            (false, true) => (Chroma::C444, true),
        };
        fn expand<T: Copy>(v: &[T], cw: usize, ch: usize, h: usize, rows_440: bool) -> Vec<T> {
            if !rows_440 {
                return v[..cw * ch].to_vec();
            }
            let mut out = Vec::with_capacity(cw * h);
            for y in 0..h {
                out.extend_from_slice(&v[(y >> 1) * cw..(y >> 1) * cw + cw]);
            }
            out
        }
        let data = match (&p.y, &p.u, &p.v) {
            (Plane::U8(y), Plane::U8(u), Plane::U8(v)) => PixelData::Yuv8 {
                planes: [Arc::new(y[..w * h].to_vec()), Arc::new(expand(u, cw, ch, h, rows_440)), Arc::new(expand(v, cw, ch, h, rows_440))],
                chroma,
                alpha: None,
            },
            (Plane::U16(y), Plane::U16(u), Plane::U16(v)) => PixelData::Yuv16 {
                planes: [Arc::new(y[..w * h].to_vec()), Arc::new(expand(u, cw, ch, h, rows_440)), Arc::new(expand(v, cw, ch, h, rows_440))],
                chroma,
                bits: p.bit_depth,
                alpha: None,
            },
            _ => unreachable!("VP9 planes share one sample type"),
        };
        // render_size (the intended display size) is not applied: the container's display
        // dimensions / pixel aspect describe the presentation.
        let par = (1, 1);
        DecodedFrame { pts, frame: VideoFrame { width: p.width, height: p.height, data, color, par, pts: filmcraft_time::Tick::ZERO } }
    }
}

impl VideoDecoder for Vp9Decoder {
    fn decode(&mut self, sample: &[u8], pts: i64) -> Result<Vec<DecodedFrame>> {
        let pics = self.dec.decode(sample, pts).map_err(|e| CodecError::Decode(e.to_string()))?;
        Ok(pics.into_iter().map(|p| self.convert(p)).collect())
    }
    fn flush(&mut self) -> Vec<DecodedFrame> {
        self.dec.flush().into_iter().map(|p| self.convert(p)).collect()
    }
    fn reset(&mut self) {
        self.dec.reset();
    }
    fn name(&self) -> &str {
        "FilmCraft VP9"
    }
    fn is_random_access(&self, sample: &[u8]) -> Option<bool> {
        Some(filmcraft_vp9::is_keyframe(sample))
    }
}

pub fn vp9_factory(e: &SampleEntry) -> Option<Result<Box<dyn VideoDecoder>>> {
    match &e.codec {
        CodecConfig::Vp9(c) => Some(Ok(Box::new(Vp9Decoder::new(Some(c))) as Box<dyn VideoDecoder>)),
        _ => None,
    }
}

/// Our AV1 decoder. The `av1C` configuration OBUs (sequence header) are fed before the first
/// sample and again after every reset.
pub struct Av1Decoder {
    dec: filmcraft_av1::Decoder,
    config_obus: Vec<u8>,
    primed: bool,
}

impl Av1Decoder {
    pub fn new(config_obus: Vec<u8>) -> Av1Decoder {
        Av1Decoder { dec: filmcraft_av1::Decoder::new(), config_obus, primed: false }
    }

    fn convert(p: filmcraft_av1::Picture, pts: i64) -> DecodedFrame {
        use std::sync::Arc;
        let w = p.width as usize;
        let h = p.height as usize;
        let mut color = filmcraft_color::ColorInfo::REC709;
        if let Some(m) = filmcraft_color::Matrix::from_code(p.matrix_coefficients) {
            color.matrix = m;
        } else {
            color.matrix = filmcraft_frame::default_matrix(p.width, p.height);
        }
        if let Some(t) = filmcraft_color::Transfer::from_code(p.transfer_characteristics) {
            color.transfer = t;
        }
        color.primaries = match p.color_primaries {
            9 => filmcraft_color::Primaries::Bt2020,
            12 => filmcraft_color::Primaries::P3D65,
            5 => filmcraft_color::Primaries::Bt601_625,
            6 => filmcraft_color::Primaries::Bt601_525,
            _ => filmcraft_color::Primaries::Bt709,
        };
        if p.full_range {
            color.range = filmcraft_color::Range::Full;
        }
        let chroma = match (p.subsampling_x, p.subsampling_y) {
            (1, 1) => filmcraft_frame::Chroma::C420,
            (1, 0) => filmcraft_frame::Chroma::C422,
            _ => filmcraft_frame::Chroma::C444,
        };
        let cw = (w + p.subsampling_x as usize) >> p.subsampling_x;
        let ch = (h + p.subsampling_y as usize) >> p.subsampling_y;
        let [y, mut u, mut v] = p.planes;
        if p.mono_chrome {
            u = vec![1u16 << (p.bit_depth - 1); cw * ch];
            v = u.clone();
        }
        let data = if p.bit_depth == 8 {
            let to8 = |p: Vec<u16>| Arc::new(p.into_iter().map(|v| v as u8).collect::<Vec<u8>>());
            filmcraft_frame::PixelData::Yuv8 { planes: [to8(y), to8(u), to8(v)], chroma, alpha: None }
        } else {
            filmcraft_frame::PixelData::Yuv16 { planes: [Arc::new(y), Arc::new(u), Arc::new(v)], chroma, bits: p.bit_depth as u32, alpha: None }
        };
        DecodedFrame { pts, frame: VideoFrame { width: p.width, height: p.height, data, color, par: (1, 1), pts: filmcraft_time::Tick::ZERO } }
    }
}

impl VideoDecoder for Av1Decoder {
    fn decode(&mut self, sample: &[u8], pts: i64) -> Result<Vec<DecodedFrame>> {
        if !self.primed {
            self.primed = true;
            if !self.config_obus.is_empty() {
                self.dec.decode(&self.config_obus).map_err(|e| CodecError::Decode(e.to_string()))?;
            }
        }
        // With frame threads pictures can come out of a later call; each carries its own pts.
        let pics = self.dec.decode_pts(sample, pts).map_err(|e| CodecError::Decode(e.to_string()))?;
        Ok(pics
            .into_iter()
            .map(|p| {
                let pts = p.pts;
                Self::convert(p, pts)
            })
            .collect())
    }
    fn flush(&mut self) -> Vec<DecodedFrame> {
        self.dec
            .flush()
            .into_iter()
            .map(|p| {
                let pts = p.pts;
                Self::convert(p, pts)
            })
            .collect()
    }
    fn reset(&mut self) {
        self.dec = filmcraft_av1::Decoder::new();
        self.primed = false;
    }
    fn name(&self) -> &str {
        "FilmCraft AV1"
    }
    fn is_random_access(&self, sample: &[u8]) -> Option<bool> {
        Some(filmcraft_av1::is_key_frame_unit(sample))
    }
}

pub fn av1_factory(e: &SampleEntry) -> Option<Result<Box<dyn VideoDecoder>>> {
    match &e.codec {
        CodecConfig::Av1(c) => Some(Ok(Box::new(Av1Decoder::new(c.config_obus.clone())) as Box<dyn VideoDecoder>)),
        _ => None,
    }
}

/// Our ProRes decoder (every frame is intra; slices decode in parallel).
pub struct ProResDecoder;

impl VideoDecoder for ProResDecoder {
    fn decode(&mut self, sample: &[u8], pts: i64) -> Result<Vec<DecodedFrame>> {
        use std::sync::Arc;
        let f = filmcraft_prores::decode_frame(sample).map_err(|e| CodecError::Decode(e.to_string()))?;
        let chroma = match f.chroma {
            filmcraft_prores::ChromaFormat::Yuv422 => filmcraft_frame::Chroma::C422,
            filmcraft_prores::ChromaFormat::Yuv444 => filmcraft_frame::Chroma::C444,
        };
        let mut color = filmcraft_color::ColorInfo::REC709;
        if let Some(m) = filmcraft_color::Matrix::from_code(f.color.matrix) {
            color.matrix = m;
        }
        if let Some(t) = filmcraft_color::Transfer::from_code(f.color.transfer) {
            color.transfer = t;
        }
        let frame = VideoFrame {
            width: f.width,
            height: f.height,
            data: filmcraft_frame::PixelData::Yuv16 {
                planes: [Arc::new(f.y), Arc::new(f.cb), Arc::new(f.cr)],
                chroma,
                bits: f.bit_depth as u32,
                alpha: f.alpha.map(Arc::new),
            },
            color,
            par: (1, 1),
            pts: filmcraft_time::Tick::ZERO,
        };
        Ok(vec![DecodedFrame { pts, frame }])
    }
    fn flush(&mut self) -> Vec<DecodedFrame> {
        Vec::new()
    }
    fn reset(&mut self) {}
    fn name(&self) -> &str {
        "FilmCraft ProRes"
    }
    fn intra_only(&self) -> bool {
        true
    }
}

pub fn prores_factory(e: &SampleEntry) -> Option<Result<Box<dyn VideoDecoder>>> {
    matches!(e.codec, CodecConfig::ProRes { .. }).then(|| Ok(Box::new(ProResDecoder) as Box<dyn VideoDecoder>))
}

/// Our DNxHD / DNxHR (VC-3) decoder (every frame is intra; macroblock rows decode in parallel).
pub struct DnxDecoder;

/// Convert a decoded VC-3 frame to a [`VideoFrame`]. RGB (4:4:4) frames are converted to
/// BT.709 video-range Y'CbCr 4:4:4 at the coded depth.
pub fn dnx_to_video_frame(f: filmcraft_dnx::Frame) -> VideoFrame {
    use std::sync::Arc;
    let chroma = match f.chroma {
        filmcraft_dnx::ChromaFormat::Yuv420 => filmcraft_frame::Chroma::C420,
        filmcraft_dnx::ChromaFormat::Yuv422 => filmcraft_frame::Chroma::C422,
        filmcraft_dnx::ChromaFormat::Yuv444 => filmcraft_frame::Chroma::C444,
    };
    let mut color = filmcraft_color::ColorInfo::REC709;
    if matches!(f.color_volume, filmcraft_dnx::ColorVolume::Bt2020Ncl | filmcraft_dnx::ColorVolume::Bt2020Cl) {
        color.matrix = filmcraft_color::Matrix::Bt2020Ncl;
        color.primaries = filmcraft_color::Primaries::Bt2020;
    }
    let (mut y, mut cb, mut cr) = (f.y, f.cb, f.cr);
    if f.rgb {
        // planes hold G, B, R (video range); derive Y'CbCr with the stream's matrix
        let (kr, kb) = color.matrix.kr_kb();
        let kg = 1.0 - kr - kb;
        let s = (1u32 << (f.bit_depth - 8)) as f32;
        let max = ((1u32 << f.bit_depth) - 1) as f32;
        let c = 224.0 / 219.0;
        for i in 0..y.len() {
            let (g, b, r) = (y[i] as f32, cb[i] as f32, cr[i] as f32);
            let yy = kr * r + kg * g + kb * b;
            let u = (b - yy) / (2.0 * (1.0 - kb)) * c + 128.0 * s;
            let v = (r - yy) / (2.0 * (1.0 - kr)) * c + 128.0 * s;
            y[i] = (yy + 0.5).clamp(0.0, max) as u16;
            cb[i] = (u + 0.5).clamp(0.0, max) as u16;
            cr[i] = (v + 0.5).clamp(0.0, max) as u16;
        }
    }
    let par = match f.par {
        (n, d) if n > 0 && d > 0 => (n as u32, d as u32),
        // thin rasters (1440 / 960 wide) are anamorphic 16:9
        _ if matches!(f.cid, 1244 | 1259 | 1260) => (4, 3),
        _ if f.cid == 1258 => (4, 3),
        _ => (1, 1),
    };
    let data = if f.bit_depth == 8 {
        let to8 = |p: Vec<u16>| Arc::new(p.into_iter().map(|v| v as u8).collect::<Vec<u8>>());
        filmcraft_frame::PixelData::Yuv8 { planes: [to8(y), to8(cb), to8(cr)], chroma, alpha: f.alpha.map(to8) }
    } else {
        filmcraft_frame::PixelData::Yuv16 { planes: [Arc::new(y), Arc::new(cb), Arc::new(cr)], chroma, bits: f.bit_depth as u32, alpha: f.alpha.map(Arc::new) }
    };
    VideoFrame { width: f.width, height: f.height, data, color, par, pts: filmcraft_time::Tick::ZERO }
}

impl VideoDecoder for DnxDecoder {
    fn decode(&mut self, sample: &[u8], pts: i64) -> Result<Vec<DecodedFrame>> {
        let f = filmcraft_dnx::decode_frame(sample).map_err(|e| CodecError::Decode(e.to_string()))?;
        Ok(vec![DecodedFrame { pts, frame: dnx_to_video_frame(f) }])
    }
    fn flush(&mut self) -> Vec<DecodedFrame> {
        Vec::new()
    }
    fn reset(&mut self) {}
    fn name(&self) -> &str {
        "FilmCraft DNxHD/DNxHR"
    }
    fn intra_only(&self) -> bool {
        true
    }
}

pub fn dnx_factory(e: &SampleEntry) -> Option<Result<Box<dyn VideoDecoder>>> {
    matches!(e.codec, CodecConfig::Dnx { .. }).then(|| Ok(Box::new(DnxDecoder) as Box<dyn VideoDecoder>))
}
