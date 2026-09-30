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
}

pub fn prores_factory(e: &SampleEntry) -> Option<Result<Box<dyn VideoDecoder>>> {
    matches!(e.codec, CodecConfig::ProRes { .. }).then(|| Ok(Box::new(ProResDecoder) as Box<dyn VideoDecoder>))
}
