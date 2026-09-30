//! The container + codec hub.
//!
//! - [`VideoDecoder`]: the trait every video codec implements (our own H.264/ProRes/…, MJPEG, and
//!   OS hardware decoders registered by the platform layer). Factories are tried in registration
//!   order, so a hardware decoder can take precedence over the pure-Rust one.
//! - [`Mp4Source`]: a [`MediaSource`](filmcraft_media::MediaSource) over MP4/MOV using
//!   `filmcraft-isobmff`: GOP-aware random access (seek to the preceding sync sample and decode
//!   forward, caching every decoded frame of the GOP), sequential fast path for playback, and
//!   packet-cached audio decoding.
//! - [`AudioFileSource`]: standalone compressed audio files (MP3, FLAC, Ogg Vorbis, …).
//! - [`openers`]: the openers to register with the engine's media pool.

pub mod audio;
pub mod mp4;
pub mod video;

use std::sync::{Arc, RwLock};

pub use audio::AudioFileSource;
pub use mp4::Mp4Source;
pub use video::{DecodedFrame, VideoDecoder, VideoDecoderFactory};

#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    #[error("unsupported codec: {0}")]
    Unsupported(String),
    #[error("decode error: {0}")]
    Decode(String),
    #[error("container: {0}")]
    Container(String),
}

pub type Result<T> = std::result::Result<T, CodecError>;

impl From<CodecError> for filmcraft_media::MediaError {
    fn from(e: CodecError) -> Self {
        match e {
            CodecError::Unsupported(s) => filmcraft_media::MediaError::Unsupported(s),
            other => filmcraft_media::MediaError::Decode(other.to_string()),
        }
    }
}

fn factories() -> &'static RwLock<Vec<VideoDecoderFactory>> {
    static F: std::sync::OnceLock<RwLock<Vec<VideoDecoderFactory>>> = std::sync::OnceLock::new();
    F.get_or_init(|| RwLock::new(vec![video::h264_factory, video::hevc_factory, video::prores_factory, video::mjpeg_factory]))
}

/// Register a video decoder factory (tried before previously registered ones).
pub fn register_video_decoder(f: VideoDecoderFactory) {
    let mut g = factories().write().unwrap_or_else(|e| e.into_inner());
    if !g.iter().any(|x| std::ptr::fn_addr_eq(*x, f)) {
        g.insert(0, f);
    }
}

/// Create a decoder for a sample entry.
pub fn make_video_decoder(entry: &filmcraft_isobmff::SampleEntry) -> Result<Box<dyn VideoDecoder>> {
    let g = factories().read().unwrap_or_else(|e| e.into_inner());
    for f in g.iter() {
        if let Some(r) = f(entry) {
            return r;
        }
    }
    Err(CodecError::Unsupported(format!("no decoder for {} video", entry.codec.name())))
}

/// Openers for the engine's media pool (MP4/MOV, standalone audio).
pub fn openers() -> Vec<filmcraft_media::Opener> {
    vec![mp4::opener, audio::opener]
}

/// Convenience: an `Arc` media source from bytes (tries MP4/MOV then audio files).
pub fn open_bytes(name: &str, bytes: Arc<[u8]>) -> std::result::Result<filmcraft_media::SharedSource, filmcraft_media::MediaError> {
    filmcraft_media::open_bytes(name, bytes, &openers())
}

#[cfg(test)]
mod tests;
