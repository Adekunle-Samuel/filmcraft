//! Export: render a sequence range and encode it to a file.
//!
//! Frames are rendered in parallel batches (one frame per core, each frame itself row-parallel),
//! then encoded and muxed in order; audio is mixed per batch and interleaved. Progress and cancel
//! are shared atomics so the UI (Export mode, header progress) and MCP can observe/cancel jobs.
//!
//! Video encoders implement [`VideoEncoder`]; codec crates register theirs with
//! [`register_encoder`] (H.264, ProRes …). Built in: Motion-JPEG (MOV), PNG sequence, GIF, WAV.

use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use filmcraft_isobmff::{Brand, Mp4Writer, PcmConfig, SampleEntry, TrackConfig, WriteSample, WriterOptions};
use filmcraft_project::{ItemId, Project};
use filmcraft_render::{RenderOptions, SourceProvider};
use filmcraft_time::{FrameRate, Tick, TimeRange};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    #[error("no such sequence")]
    NoSequence,
    #[error("unsupported: {0}")]
    Unsupported(String),
    #[error("I/O: {0}")]
    Io(String),
    #[error("encode: {0}")]
    Encode(String),
    #[error("cancelled")]
    Cancelled,
}

pub type Result<T> = std::result::Result<T, ExportError>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Format {
    /// MPEG-4, H.264 video + AAC audio (needs the H.264/AAC encoders registered).
    H264,
    /// QuickTime, Apple ProRes 422 HQ + PCM (needs the ProRes encoder registered).
    ProRes,
    /// QuickTime, Motion-JPEG + 16-bit PCM.
    Mjpeg,
    PngSequence,
    Gif,
    Wav,
}

impl Format {
    pub fn from_name(s: &str) -> Option<Format> {
        Some(match s.to_ascii_lowercase().replace([' ', '-', '_', '.'], "").as_str() {
            "h264" | "mp4" | "avc" => Format::H264,
            "prores" | "mov" => Format::ProRes,
            "mjpeg" | "motionjpeg" | "jpeg" => Format::Mjpeg,
            "png" | "pngsequence" => Format::PngSequence,
            "gif" | "animatedgif" => Format::Gif,
            "wav" | "waveform" => Format::Wav,
            _ => return None,
        })
    }
    pub fn extension(self) -> &'static str {
        match self {
            Format::H264 => "mp4",
            Format::ProRes | Format::Mjpeg => "mov",
            Format::PngSequence => "png",
            Format::Gif => "gif",
            Format::Wav => "wav",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Format::H264 => "H.264",
            Format::ProRes => "Apple ProRes",
            Format::Mjpeg => "QuickTime (Motion JPEG)",
            Format::PngSequence => "PNG Sequence",
            Format::Gif => "Animated GIF",
            Format::Wav => "Waveform Audio",
        }
    }
    pub const ALL: [Format; 6] = [Format::H264, Format::ProRes, Format::Mjpeg, Format::PngSequence, Format::Gif, Format::Wav];
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExportSettings {
    pub format: Format,
    pub path: String,
    /// Timeline range (default: In/Out if set, else the whole sequence).
    pub range: Option<TimeRange>,
    /// Output scale (1.0 = sequence frame size).
    pub scale: f32,
    pub include_audio: bool,
    /// Quality 0–100 for lossy codecs.
    pub quality: u8,
    /// Target video bitrate (kbps) for bitrate-driven encoders.
    pub bitrate_kbps: u32,
}

impl Default for ExportSettings {
    fn default() -> Self {
        Self { format: Format::H264, path: String::new(), range: None, scale: 1.0, include_audio: true, quality: 90, bitrate_kbps: 20_000 }
    }
}

/// Shared progress/cancel state of an export job.
#[derive(Default)]
pub struct Progress {
    pub done: AtomicU64,
    pub total: AtomicU64,
    pub cancel: AtomicBool,
    pub finished: AtomicBool,
    pub status: Mutex<String>,
    pub error: Mutex<Option<String>>,
}

impl Progress {
    pub fn fraction(&self) -> f32 {
        let t = self.total.load(Ordering::Relaxed).max(1);
        self.done.load(Ordering::Relaxed) as f32 / t as f32
    }
    fn set_status(&self, s: impl Into<String>) {
        *self.status.lock().unwrap_or_else(|e| e.into_inner()) = s.into();
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Report {
    pub path: String,
    pub frames: u64,
    pub seconds: f64,
    pub bytes: u64,
    pub render_fps: f64,
}

/// A packet produced by a video encoder.
pub struct EncodedPacket {
    pub data: Vec<u8>,
    pub key: bool,
    /// Duration in encoder timescale units.
    pub duration: u32,
    /// pts − dts in encoder timescale units.
    pub composition_offset: i32,
}

/// Input picture for encoders: straight sRGB RGBA8 (encoders convert to their own YUV).
pub struct EncoderFrame<'a> {
    pub width: u32,
    pub height: u32,
    pub rgba: &'a [u8],
    pub index: u64,
}

pub trait VideoEncoder: Send {
    /// MP4/MOV sample entry (codec config) — may only be complete after the first frame.
    fn sample_entry(&self) -> SampleEntry;
    fn timescale(&self) -> u32;
    fn encode(&mut self, frame: &EncoderFrame) -> Result<Vec<EncodedPacket>>;
    fn flush(&mut self) -> Result<Vec<EncodedPacket>>;
    /// Media start offset for an edit list (B-frame delay), in the encoder timescale.
    fn media_start(&self) -> Option<i64> {
        None
    }
}

/// Audio encoder (AAC) plugged in by codec crates; PCM is built in.
pub trait AudioEncoder: Send {
    fn sample_entry(&self) -> SampleEntry;
    fn encode(&mut self, planar: &[Vec<f32>]) -> Result<Vec<Vec<u8>>>;
    fn flush(&mut self) -> Result<Vec<Vec<u8>>>;
    /// Encoder delay (priming) in samples.
    fn priming(&self) -> u32;
    /// Samples per access unit (1024 for AAC).
    fn frame_size(&self) -> u32;
}

pub type EncoderFactory = fn(format: Format, width: u32, height: u32, rate: FrameRate, settings: &ExportSettings) -> Option<Result<Box<dyn VideoEncoder>>>;
pub type AudioEncoderFactory = fn(format: Format, sample_rate: u32, channels: u32, settings: &ExportSettings) -> Option<Result<Box<dyn AudioEncoder>>>;

fn video_factories() -> &'static RwLock<Vec<EncoderFactory>> {
    static F: OnceLock<RwLock<Vec<EncoderFactory>>> = OnceLock::new();
    F.get_or_init(|| RwLock::new(vec![prores_factory, mjpeg_factory]))
}
fn audio_factories() -> &'static RwLock<Vec<AudioEncoderFactory>> {
    static F: OnceLock<RwLock<Vec<AudioEncoderFactory>>> = OnceLock::new();
    F.get_or_init(|| RwLock::new(vec![aac_factory]))
}

pub fn register_encoder(f: EncoderFactory) {
    video_factories().write().unwrap_or_else(|e| e.into_inner()).insert(0, f);
}
pub fn register_audio_encoder(f: AudioEncoderFactory) {
    audio_factories().write().unwrap_or_else(|e| e.into_inner()).insert(0, f);
}

/// Whether a format can currently be exported (its encoder is registered).
pub fn available(format: Format) -> bool {
    match format {
        Format::PngSequence | Format::Gif | Format::Wav | Format::Mjpeg => true,
        f => {
            let s = ExportSettings { format: f, ..Default::default() };
            video_factories().read().unwrap_or_else(|e| e.into_inner()).iter().any(|fac| fac(f, 64, 64, FrameRate::FPS_24, &s).is_some())
        }
    }
}

struct MjpegEncoder {
    w: u16,
    h: u16,
    quality: u8,
    rate: FrameRate,
}

impl VideoEncoder for MjpegEncoder {
    fn sample_entry(&self) -> SampleEntry {
        SampleEntry::jpeg(self.w, self.h)
    }
    fn timescale(&self) -> u32 {
        self.rate.num as u32
    }
    fn encode(&mut self, f: &EncoderFrame) -> Result<Vec<EncodedPacket>> {
        let rgb: Vec<u8> = f.rgba.chunks_exact(4).flat_map(|p| [p[0], p[1], p[2]]).collect();
        let mut out = Vec::new();
        let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, self.quality);
        enc.encode(&rgb, f.width, f.height, image::ExtendedColorType::Rgb8).map_err(|e| ExportError::Encode(e.to_string()))?;
        Ok(vec![EncodedPacket { data: out, key: true, duration: self.rate.den as u32, composition_offset: 0 }])
    }
    fn flush(&mut self) -> Result<Vec<EncodedPacket>> {
        Ok(Vec::new())
    }
}

fn mjpeg_factory(format: Format, w: u32, h: u32, rate: FrameRate, s: &ExportSettings) -> Option<Result<Box<dyn VideoEncoder>>> {
    (format == Format::Mjpeg).then(|| Ok(Box::new(MjpegEncoder { w: w as u16, h: h as u16, quality: s.quality.clamp(1, 100), rate }) as Box<dyn VideoEncoder>))
}

/// AAC-LC (our encoder), 320 kbps stereo by default.
struct AacEncoder {
    enc: filmcraft_aac::Encoder,
    rate: u32,
    channels: u32,
}

impl AudioEncoder for AacEncoder {
    fn sample_entry(&self) -> SampleEntry {
        SampleEntry::aac(self.enc.audio_specific_config(), self.channels, self.rate)
    }
    fn encode(&mut self, planar: &[Vec<f32>]) -> Result<Vec<Vec<u8>>> {
        let refs: Vec<&[f32]> = planar.iter().map(Vec::as_slice).collect();
        Ok(self.enc.encode(&refs))
    }
    fn flush(&mut self) -> Result<Vec<Vec<u8>>> {
        Ok(self.enc.flush())
    }
    fn priming(&self) -> u32 {
        self.enc.priming_samples()
    }
    fn frame_size(&self) -> u32 {
        1024
    }
}

fn aac_factory(_format: Format, sample_rate: u32, channels: u32, _s: &ExportSettings) -> Option<Result<Box<dyn AudioEncoder>>> {
    Some(
        filmcraft_aac::Encoder::new(filmcraft_aac::EncoderConfig::cbr(sample_rate, channels as usize, 320_000))
            .map(|enc| Box::new(AacEncoder { enc, rate: sample_rate, channels }) as Box<dyn AudioEncoder>)
            .map_err(|e| ExportError::Encode(e.to_string())),
    )
}

/// ProRes 422 HQ encoder: sRGB/709 RGBA8 → 10-bit limited-range BT.709 4:2:2.
struct ProResEncoder {
    enc: filmcraft_prores::Encoder,
    w: u32,
    h: u32,
    rate: FrameRate,
}

impl VideoEncoder for ProResEncoder {
    fn sample_entry(&self) -> SampleEntry {
        SampleEntry::prores(filmcraft_isobmff::FourCc(*b"apch"), self.w as u16, self.h as u16)
    }
    fn timescale(&self) -> u32 {
        self.rate.num as u32
    }
    fn encode(&mut self, f: &EncoderFrame) -> Result<Vec<EncodedPacket>> {
        let mut fr = filmcraft_prores::Frame::new(f.width, f.height, filmcraft_prores::ChromaFormat::Yuv422, 10, false);
        rgba_to_yuv422_10(f.rgba, f.width as usize, f.height as usize, &mut fr.y, &mut fr.cb, &mut fr.cr);
        let data = self.enc.encode(&fr).map_err(|e| ExportError::Encode(e.to_string()))?;
        Ok(vec![EncodedPacket { data, key: true, duration: self.rate.den as u32, composition_offset: 0 }])
    }
    fn flush(&mut self) -> Result<Vec<EncodedPacket>> {
        Ok(Vec::new())
    }
}

/// BT.709 limited-range 10-bit 4:2:2 from straight RGBA8 (chroma averaged horizontally).
pub fn rgba_to_yuv422_10(rgba: &[u8], w: usize, h: usize, y: &mut [u16], cb: &mut [u16], cr: &mut [u16]) {
    let cw = w.div_ceil(2);
    debug_assert!(y.len() >= w * h && rgba.len() >= w * h * 4);
    y.par_chunks_mut(w).zip(cb.par_chunks_mut(cw).zip(cr.par_chunks_mut(cw))).enumerate().for_each(|(row, (yr, (cbr, crr)))| {
        let src = &rgba[row * w * 4..(row + 1) * w * 4];
        let mut us = vec![0f32; w];
        let mut vs = vec![0f32; w];
        for x in 0..w {
            let (r, g, b) = (src[x * 4] as f32 / 255.0, src[x * 4 + 1] as f32 / 255.0, src[x * 4 + 2] as f32 / 255.0);
            let yy = 0.2126 * r + 0.7152 * g + 0.0722 * b;
            yr[x] = (64.0 + 876.0 * yy).round().clamp(4.0, 1019.0) as u16;
            us[x] = (b - yy) / 1.8556;
            vs[x] = (r - yy) / 1.5748;
        }
        for cx in 0..cw {
            let a = cx * 2;
            let b2 = (a + 1).min(w - 1);
            let u = (us[a] + us[b2]) * 0.5;
            let v = (vs[a] + vs[b2]) * 0.5;
            cbr[cx] = (512.0 + 896.0 * u).round().clamp(4.0, 1019.0) as u16;
            crr[cx] = (512.0 + 896.0 * v).round().clamp(4.0, 1019.0) as u16;
        }
    });
}

fn prores_factory(format: Format, w: u32, h: u32, rate: FrameRate, _s: &ExportSettings) -> Option<Result<Box<dyn VideoEncoder>>> {
    (format == Format::ProRes)
        .then(|| Ok(Box::new(ProResEncoder { enc: filmcraft_prores::Encoder::new(filmcraft_prores::Profile::Hq, w, h), w, h, rate }) as Box<dyn VideoEncoder>))
}

/// The range to export (settings → In/Out → whole sequence).
pub fn export_range(project: &Project, seq: ItemId, settings: &ExportSettings) -> Result<TimeRange> {
    let q = project.sequence(seq).ok_or(ExportError::NoSequence)?;
    if let Some(r) = settings.range {
        return Ok(r);
    }
    let fd = q.settings.frame_rate.frame_duration();
    let a = q.mark_in.unwrap_or(Tick::ZERO);
    let b = q.mark_out.map(|o| o + fd).unwrap_or(q.duration());
    Ok(TimeRange::from_bounds(a, b.max(a + fd)))
}

/// Run an export (blocking; call from a worker thread).
pub fn export(project: &Arc<Project>, seq: ItemId, settings: &ExportSettings, sources: &dyn SourceProvider, progress: &Progress) -> Result<Report> {
    let t0 = std::time::Instant::now();
    let q = project.sequence(seq).ok_or(ExportError::NoSequence)?;
    let rate = q.settings.frame_rate;
    let range = export_range(project, seq, settings)?;
    let f0 = rate.frame_at(range.start);
    let f1 = rate.frame_at(range.end() - Tick(1)) + 1;
    let nframes = (f1 - f0).max(0) as u64;
    let w = (((q.settings.width as f32 * settings.scale).round() as u32).max(2)) & !1;
    let h = (((q.settings.height as f32 * settings.scale).round() as u32).max(2)) & !1;
    progress.total.store(if settings.format == Format::Wav { 1 } else { nframes }, Ordering::Relaxed);
    progress.set_status(format!("Exporting {} frames ({})", nframes, settings.format.label()));
    let opts = RenderOptions { scale: w as f32 / q.settings.width as f32, ..Default::default() };
    let render = |f: i64| -> Vec<u8> {
        let img = filmcraft_render::render_sequence(project, seq, rate.tick_of(f), opts, sources);
        let mut rgba = img.over_black_rgba8();
        if img.w as u32 != w || img.h as u32 != h {
            // even-size crop/pad
            let mut out = vec![0u8; (w * h * 4) as usize];
            for y in 0..(h as usize).min(img.h) {
                let n = (w as usize).min(img.w) * 4;
                out[y * w as usize * 4..y * w as usize * 4 + n].copy_from_slice(&rgba[y * img.w * 4..y * img.w * 4 + n]);
            }
            rgba = out;
        }
        rgba
    };
    let batch = rayon::current_num_threads().clamp(2, 16) as i64;
    let sr = q.settings.sample_rate;
    let bytes = match settings.format {
        Format::Wav => {
            let n = range.duration.to_units_floor(sr as i64) as usize;
            let buf = filmcraft_render::audio::mix_sequence(project, q, range.start.to_units_floor(sr as i64), n, sources);
            let data = filmcraft_media::wav::write_wav16(&buf.interleaved(), 2, sr);
            std::fs::write(&settings.path, &data).map_err(|e| ExportError::Io(e.to_string()))?;
            progress.done.store(1, Ordering::Relaxed);
            data.len() as u64
        }
        Format::PngSequence => {
            let base = settings.path.trim_end_matches(".png").to_string();
            let mut total = 0u64;
            let mut f = f0;
            while f < f1 {
                if progress.cancel.load(Ordering::Relaxed) {
                    return Err(ExportError::Cancelled);
                }
                let end = (f + batch).min(f1);
                let written: Vec<Result<u64>> = (f..end)
                    .into_par_iter()
                    .map(|fi| {
                        let rgba = render(fi);
                        let path = format!("{base}_{:05}.png", fi - f0);
                        image::save_buffer(&path, &rgba, w, h, image::ExtendedColorType::Rgba8).map_err(|e| ExportError::Io(e.to_string()))?;
                        Ok(std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0))
                    })
                    .collect();
                for r in written {
                    total += r?;
                }
                progress.done.fetch_add((end - f) as u64, Ordering::Relaxed);
                f = end;
            }
            total
        }
        Format::Gif => {
            let file = std::fs::File::create(&settings.path).map_err(|e| ExportError::Io(e.to_string()))?;
            let mut enc = image::codecs::gif::GifEncoder::new_with_speed(std::io::BufWriter::new(file), 10);
            enc.set_repeat(image::codecs::gif::Repeat::Infinite).map_err(|e| ExportError::Encode(e.to_string()))?;
            let delay = image::Delay::from_numer_denom_ms((1000 * rate.den) as u32, rate.num as u32);
            let mut f = f0;
            while f < f1 {
                if progress.cancel.load(Ordering::Relaxed) {
                    return Err(ExportError::Cancelled);
                }
                let end = (f + batch).min(f1);
                let frames: Vec<Vec<u8>> = (f..end).into_par_iter().map(render).collect();
                for rgba in frames {
                    let img = image::RgbaImage::from_raw(w, h, rgba).ok_or_else(|| ExportError::Encode("frame".into()))?;
                    enc.encode_frame(image::Frame::from_parts(img, 0, 0, delay)).map_err(|e| ExportError::Encode(e.to_string()))?;
                }
                progress.done.fetch_add((end - f) as u64, Ordering::Relaxed);
                f = end;
            }
            drop(enc);
            std::fs::metadata(&settings.path).map(|m| m.len()).unwrap_or(0)
        }
        Format::H264 | Format::ProRes | Format::Mjpeg => {
            let mut venc = video_factories()
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .find_map(|fac| fac(settings.format, w, h, rate, settings))
                .ok_or_else(|| ExportError::Unsupported(format!("{} encoder not available yet", settings.format.label())))??;
            let brand = if settings.format == Format::H264 { Brand::Mp4 } else { Brand::Mov };
            let file = std::fs::File::create(&settings.path).map_err(|e| ExportError::Io(e.to_string()))?;
            // Encode the first batch before creating tracks (encoders may finalise codec config then).
            let mut pending: Vec<EncodedPacket> = Vec::new();
            let first_end = (f0 + batch).min(f1);
            let first: Vec<Vec<u8>> = (f0..first_end).into_par_iter().map(render).collect();
            for (k, rgba) in first.iter().enumerate() {
                pending.extend(venc.encode(&EncoderFrame { width: w, height: h, rgba, index: k as u64 })?);
            }
            progress.done.fetch_add((first_end - f0) as u64, Ordering::Relaxed);
            let mut mux = Mp4Writer::new(std::io::BufWriter::new(file), WriterOptions::new(brand)).map_err(|e| ExportError::Io(e.to_string()))?;
            let mut vcfg = TrackConfig::new(venc.sample_entry(), venc.timescale());
            vcfg.media_start = venc.media_start();
            let vt = mux.add_track(vcfg).map_err(|e| ExportError::Io(e.to_string()))?;
            // audio: AAC for MP4 when available, PCM otherwise (MOV)
            let mut aenc: Option<Box<dyn AudioEncoder>> = if settings.include_audio && brand == Brand::Mp4 {
                audio_factories().read().unwrap_or_else(|e| e.into_inner()).iter().find_map(|f| f(settings.format, sr, 2, settings)).transpose()?
            } else {
                None
            };
            let at = if !settings.include_audio {
                None
            } else if let Some(a) = &aenc {
                let mut c = TrackConfig::new(a.sample_entry(), sr);
                c.media_start = Some(a.priming() as i64);
                Some(mux.add_track(c).map_err(|e| ExportError::Io(e.to_string()))?)
            } else if brand == Brand::Mov {
                let pcm = PcmConfig { bits: 16, float: false, big_endian: false, signed: true, channels: 2, sample_rate: sr as f64 };
                Some(mux.add_track(TrackConfig::new(SampleEntry::pcm(pcm), sr)).map_err(|e| ExportError::Io(e.to_string()))?)
            } else {
                None
            };
            let mut audio_cursor = range.start.to_units_floor(sr as i64);
            let write_audio_until = |mux: &mut Mp4Writer<std::io::BufWriter<std::fs::File>>,
                                     until: Tick,
                                     cursor: &mut i64,
                                     aenc: &mut Option<Box<dyn AudioEncoder>>|
             -> Result<()> {
                let Some(at) = at else { return Ok(()) };
                let end = until.to_units_floor(sr as i64);
                if end <= *cursor {
                    return Ok(());
                }
                let n = (end - *cursor) as usize;
                let buf = filmcraft_render::audio::mix_sequence(project, q, *cursor, n, sources);
                *cursor = end;
                match aenc {
                    Some(a) => {
                        for au in a.encode(&buf.channels)? {
                            mux.write_sample(at, WriteSample { data: &au, duration: a.frame_size(), composition_offset: 0, is_sync: true })
                                .map_err(|e| ExportError::Io(e.to_string()))?;
                        }
                    }
                    None => {
                        let mut pcm = Vec::with_capacity(n * 4);
                        for s in buf.interleaved() {
                            pcm.extend_from_slice(&((s.clamp(-1.0, 1.0) * 32767.0).round() as i16).to_le_bytes());
                        }
                        mux.write_sample(at, WriteSample { data: &pcm, duration: n as u32, composition_offset: 0, is_sync: true })
                            .map_err(|e| ExportError::Io(e.to_string()))?;
                    }
                }
                Ok(())
            };
            for p in pending.drain(..) {
                mux.write_sample(vt, WriteSample { data: &p.data, duration: p.duration, composition_offset: p.composition_offset, is_sync: p.key })
                    .map_err(|e| ExportError::Io(e.to_string()))?;
            }
            write_audio_until(&mut mux, rate.tick_of(first_end), &mut audio_cursor, &mut aenc)?;
            let mut f = first_end;
            while f < f1 {
                if progress.cancel.load(Ordering::Relaxed) {
                    return Err(ExportError::Cancelled);
                }
                let end = (f + batch).min(f1);
                let frames: Vec<Vec<u8>> = (f..end).into_par_iter().map(render).collect();
                for (k, rgba) in frames.iter().enumerate() {
                    for p in venc.encode(&EncoderFrame { width: w, height: h, rgba, index: (f - f0) as u64 + k as u64 })? {
                        mux.write_sample(vt, WriteSample { data: &p.data, duration: p.duration, composition_offset: p.composition_offset, is_sync: p.key })
                            .map_err(|e| ExportError::Io(e.to_string()))?;
                    }
                }
                write_audio_until(&mut mux, rate.tick_of(end), &mut audio_cursor, &mut aenc)?;
                progress.done.fetch_add((end - f) as u64, Ordering::Relaxed);
                f = end;
            }
            for p in venc.flush()? {
                mux.write_sample(vt, WriteSample { data: &p.data, duration: p.duration, composition_offset: p.composition_offset, is_sync: p.key })
                    .map_err(|e| ExportError::Io(e.to_string()))?;
            }
            write_audio_until(&mut mux, range.end(), &mut audio_cursor, &mut aenc)?;
            if let (Some(a), Some(at)) = (aenc.as_mut(), at) {
                for au in a.flush()? {
                    mux.write_sample(at, WriteSample { data: &au, duration: a.frame_size(), composition_offset: 0, is_sync: true })
                        .map_err(|e| ExportError::Io(e.to_string()))?;
                }
            }
            let mut w = mux.finish().map_err(|e| ExportError::Io(e.to_string()))?;
            w.flush().map_err(|e| ExportError::Io(e.to_string()))?;
            std::fs::metadata(&settings.path).map(|m| m.len()).unwrap_or(0)
        }
    };
    let secs = t0.elapsed().as_secs_f64();
    progress.finished.store(true, Ordering::Relaxed);
    progress.set_status(format!("Done in {secs:.1}s"));
    Ok(Report { path: settings.path.clone(), frames: nframes, seconds: secs, bytes, render_fps: nframes as f64 / secs.max(1e-6) })
}

#[cfg(test)]
mod tests;
