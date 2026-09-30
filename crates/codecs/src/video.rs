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
