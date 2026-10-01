//! Matroska/WebM media source (`filmcraft-matroska` demux, GOP-aware video via [`crate::gop`]).
//!
//! Video codecs are mapped onto ISO-BMFF sample entries so the same decoder factories serve both
//! containers (H.264, HEVC, VP9, ProRes, MJPEG; AV1 once its decoder lands). Audio: AAC via our
//! decoder, PCM directly, MP3/FLAC/Vorbis via the bootstrap decoders.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use filmcraft_color::{ColorInfo, Matrix, Primaries, Range, Transfer};
use filmcraft_frame::{AudioBuffer, VideoFrame};
use filmcraft_isobmff::{AvcConfig, CodecConfig, FourCc, HevcConfig, PcmConfig, SampleEntry, VpcConfig};
use filmcraft_matroska::{Codec, MkvFile, TrackKind};
use filmcraft_media::{AudioStreamInfo, FrameRequest, MediaError, MediaInfo, MediaKind, MediaSource, SharedSource, VideoStreamInfo};
use filmcraft_time::{FrameRate, Tick};

use crate::audio::{PacketDecoder, decode_pcm};
use crate::gop::{GopCache, VideoSamples};
use crate::video::VideoDecoder;
use crate::{CodecError, make_video_decoder};

pub fn sniff(b: &[u8]) -> bool {
    b.len() >= 4 && b[..4] == [0x1A, 0x45, 0xDF, 0xA3]
}

struct AudioState {
    decoder: Option<PacketDecoder>,
    packets: HashMap<usize, Arc<Vec<Vec<f32>>>>,
    order: Vec<usize>,
    last_decoded: Option<usize>,
}

pub struct MkvSource {
    info: MediaInfo,
    bytes: Arc<[u8]>,
    file: MkvFile,
    vtrack: Option<usize>,
    atrack: Option<usize>,
    /// Video codec as an ISO-BMFF sample entry (for the decoder factories).
    ventry: Option<SampleEntry>,
    video: GopCache,
    audio: Mutex<AudioState>,
    /// Audio packet start positions in source sample frames.
    audio_starts: Vec<i64>,
}

/// Seconds per track timestamp unit as a rational.
fn tb(t: &filmcraft_matroska::Track) -> (i64, i64) {
    (t.timebase.0.max(1) as i64, t.timebase.1.max(1) as i64)
}

fn to_tick(t: &filmcraft_matroska::Track, pts: i64) -> Tick {
    let (n, d) = tb(t);
    Tick::from_rational(pts * n, 1, d)
}

fn from_tick(t: &filmcraft_matroska::Track, time: Tick) -> i64 {
    let (n, d) = tb(t);
    time.to_rational_floor(n, d)
}

/// VP9 configuration from the Matroska `CodecPrivate` feature list (ID / length / value triples:
/// 1 profile, 2 level, 3 bit depth, 4 chroma subsampling) and the track's `Colour`.
fn vp9_config(private: &[u8], v: Option<&filmcraft_matroska::VideoInfo>) -> VpcConfig {
    let mut c = VpcConfig { bit_depth: 8, colour_primaries: 2, transfer_characteristics: 2, matrix_coefficients: 2, ..Default::default() };
    let mut p = 0;
    while p + 2 <= private.len() {
        let (id, len) = (private[p], private[p + 1] as usize);
        let Some(val) = private.get(p + 2..p + 2 + len) else { break };
        let x = val.first().copied().unwrap_or(0);
        match id {
            1 => c.profile = x,
            2 => c.level = x,
            3 => c.bit_depth = x,
            4 => c.chroma_subsampling = x,
            _ => {}
        }
        p += 2 + len;
    }
    if let Some(col) = v.and_then(|v| v.colour.as_ref()) {
        if let Some(t) = col.transfer_characteristics {
            c.transfer_characteristics = t as u8;
        }
        if let Some(pr) = col.primaries {
            c.colour_primaries = pr as u8;
        }
        if let Some(m) = col.matrix_coefficients {
            c.matrix_coefficients = m as u8;
        }
        c.full_range = col.full_range();
    }
    c
}

fn sample_entry(c: &Codec, v: Option<&filmcraft_matroska::VideoInfo>, w: u16, h: u16) -> Option<SampleEntry> {
    Some(match c {
        Codec::Vp9 { private } => SampleEntry::video(FourCc(*b"vp09"), CodecConfig::Vp9(vp9_config(private, v)), w, h),
        Codec::Avc { avcc } => SampleEntry::avc(AvcConfig::parse(avcc).ok()?, w, h),
        Codec::Hevc { hvcc } => SampleEntry::hevc(HevcConfig::parse(hvcc).ok()?, w, h),
        Codec::ProRes { fourcc } => SampleEntry::prores(FourCc(fourcc.unwrap_or(*b"apcn")), w, h),
        Codec::Mjpeg => SampleEntry::jpeg(w, h),
        _ => return None,
    })
}

fn color_of(v: &filmcraft_matroska::VideoInfo, w: u32, h: u32) -> (ColorInfo, bool) {
    let mut c = ColorInfo { matrix: filmcraft_frame::default_matrix(w, h), transfer: Transfer::Bt709, primaries: Primaries::Bt709, range: Range::Limited };
    let Some(col) = &v.colour else { return (c, false) };
    let mut explicit = false;
    if let Some(m) = col.matrix_coefficients.and_then(|m| Matrix::from_code(m as u8)) {
        c.matrix = m;
        explicit = true;
    }
    if let Some(t) = col.transfer_characteristics.and_then(|t| Transfer::from_code(t as u8)) {
        c.transfer = t;
        explicit = true;
    }
    if let Some(p) = col.primaries {
        c.primaries = match p {
            9 => Primaries::Bt2020,
            12 => Primaries::P3D65,
            5 => Primaries::Bt601_625,
            6 => Primaries::Bt601_525,
            _ => Primaries::Bt709,
        };
    }
    if col.full_range() {
        c.range = Range::Full;
        explicit = true;
    }
    (c, explicit)
}

fn codec_label(c: &Codec) -> String {
    match c {
        Codec::Avc { .. } => "H.264".into(),
        Codec::Hevc { .. } => "HEVC".into(),
        Codec::Vp8 => "VP8".into(),
        Codec::Vp9 { .. } => "VP9".into(),
        Codec::Av1 { .. } => "AV1".into(),
        Codec::ProRes { .. } => "Apple ProRes".into(),
        Codec::Mjpeg => "Motion JPEG".into(),
        Codec::Aac { .. } => "AAC".into(),
        Codec::Opus { .. } => "Opus".into(),
        Codec::Vorbis { .. } => "Vorbis".into(),
        Codec::Flac { .. } => "FLAC".into(),
        Codec::Pcm { bits, float, .. } => format!("PCM {bits}-bit{}", if *float { " float" } else { "" }),
        Codec::Mp3 => "MP3".into(),
        other => other.name().to_string(),
    }
}

impl MkvSource {
    pub fn open(name: &str, bytes: Arc<[u8]>) -> crate::Result<Self> {
        let file = filmcraft_matroska::open(&bytes[..]).map_err(|e| CodecError::Container(e.to_string()))?;
        let vtrack = file.tracks.iter().position(|t| t.kind == TrackKind::Video && !t.samples.is_empty());
        let atrack = file.tracks.iter().position(|t| t.kind == TrackKind::Audio && !t.samples.is_empty());
        if vtrack.is_none() && atrack.is_none() {
            return Err(CodecError::Unsupported("no playable tracks".into()));
        }
        let mut explicit_color = None;
        let mut ventry = None;
        let video = vtrack.map(|i| {
            let t = &file.tracks[i];
            let v = t.video.clone().unwrap_or_default();
            let (w, h) = (v.pixel_width, v.pixel_height);
            let rate = match t.default_duration_ns {
                Some(ns) if ns > 0 => FrameRate::from_f64(1e9 / ns as f64),
                _ => {
                    let mut d: Vec<i64> = t.samples.windows(2).take(240).map(|p| (p[1].pts - p[0].pts).abs()).filter(|d| *d > 0).collect();
                    d.sort_unstable();
                    let (n, dd) = tb(t);
                    let step = d.get(d.len() / 2).copied().unwrap_or(1).max(1) as f64 * n as f64 / dd as f64;
                    FrameRate::from_f64(1.0 / step)
                }
            };
            let (color, explicit) = color_of(&v, w, h);
            if explicit {
                explicit_color = Some(color);
            }
            ventry = sample_entry(&t.codec, t.video.as_ref(), w as u16, h as u16);
            let secs = file.duration_ns().unwrap_or(0) as f64 / 1e9;
            let bitrate = (secs > 0.0).then(|| (t.samples.iter().map(|s| s.size as u64).sum::<u64>() as f64 * 8.0 / secs) as u64);
            VideoStreamInfo {
                width: w,
                height: h,
                frame_rate: rate,
                par: v.pixel_aspect(),
                codec: codec_label(&t.codec),
                pixel_format: String::new(),
                color,
                has_alpha: v.alpha_mode != 0,
                bitrate,
            }
        });
        let audio = atrack.map(|i| {
            let t = &file.tracks[i];
            let a = t.audio.clone().unwrap_or_default();
            let rate = a.output_sampling_frequency.unwrap_or(a.sampling_frequency).round().max(1.0) as u32;
            AudioStreamInfo {
                sample_rate: rate,
                channels: (a.channels as u32).max(1),
                codec: codec_label(&t.codec),
                bits_per_sample: a.bit_depth.map(|b| b as u32),
            }
        });
        let duration = match file.duration_ns() {
            Some(ns) => Tick::from_rational(ns as i64, 1, 1_000_000_000),
            None => vtrack
                .or(atrack)
                .map(|i| {
                    let t = &file.tracks[i];
                    let end = t.samples.iter().map(|s| s.pts + s.duration as i64).max().unwrap_or(0);
                    to_tick(t, end)
                })
                .unwrap_or_default(),
        };
        let info = MediaInfo {
            name: name.to_string(),
            kind: if video.is_some() { MediaKind::Movie } else { MediaKind::AudioOnly },
            duration,
            video,
            audio,
            container: if file.is_webm() { "WebM".into() } else { "Matroska".into() },
            start_timecode: None,
            file_size: Some(bytes.len() as u64),
        };
        let audio_starts = atrack
            .map(|i| {
                let t = &file.tracks[i];
                let rate = info.audio.as_ref().map_or(48_000, |a| a.sample_rate) as i64;
                let (n, d) = tb(t);
                t.samples.iter().map(|s| (s.pts as i128 * n as i128 * rate as i128 / d as i128) as i64).collect()
            })
            .unwrap_or_default();
        Ok(Self {
            info,
            bytes,
            file,
            vtrack,
            atrack,
            ventry,
            video: GopCache::new(explicit_color),
            audio: Mutex::new(AudioState { decoder: None, packets: HashMap::new(), order: Vec::new(), last_decoded: None }),
            audio_starts,
        })
    }

    fn read(&self, track: usize, i: usize) -> crate::Result<Vec<u8>> {
        self.file.read_sample(&self.bytes[..], track, i).map_err(|e| CodecError::Container(e.to_string()))
    }

    fn audio_decoder(&self, c: &Codec, rate: u32) -> crate::Result<PacketDecoder> {
        use symphonia::core::codecs::{CODEC_TYPE_FLAC, CODEC_TYPE_MP3, CODEC_TYPE_VORBIS};
        match c {
            Codec::Aac { asc } => PacketDecoder::aac(asc, rate),
            Codec::Mp3 => PacketDecoder::new(CODEC_TYPE_MP3, rate, None),
            // symphonia wants the STREAMINFO block body: skip `fLaC` + the 4-byte block header.
            Codec::Flac { private } => PacketDecoder::new(CODEC_TYPE_FLAC, rate, private.get(8..42).map(<[u8]>::to_vec)),
            Codec::Vorbis { headers } if headers.len() == 3 => {
                let mut extra = headers[0].clone();
                extra.extend_from_slice(&headers[2]);
                PacketDecoder::new(CODEC_TYPE_VORBIS, rate, Some(extra))
            }
            other => Err(CodecError::Unsupported(format!("{} audio", codec_label(other)))),
        }
    }

    fn audio_packet(&self, st: &mut AudioState, i: usize) -> crate::Result<Arc<Vec<Vec<f32>>>> {
        if let Some(p) = st.packets.get(&i) {
            return Ok(p.clone());
        }
        let ti = self.atrack.expect("audio");
        let track = &self.file.tracks[ti];
        let ainfo = self.info.audio.as_ref().expect("audio info");
        let data = self.read(ti, i)?;
        let decoded = match &track.codec {
            Codec::Pcm { float, big_endian, bits } => {
                let cfg = PcmConfig {
                    bits: *bits,
                    float: *float,
                    big_endian: *big_endian,
                    signed: *bits > 8,
                    channels: ainfo.channels,
                    sample_rate: ainfo.sample_rate as f64,
                };
                decode_pcm(&data, &cfg)
            }
            c => {
                if st.decoder.is_none() {
                    st.decoder = Some(self.audio_decoder(c, ainfo.sample_rate)?);
                }
                // Non-sequential access: reset and prime with the previous packet (codec pre-roll).
                if st.last_decoded.is_none_or(|l| l + 1 != i) {
                    let d = st.decoder.as_mut().expect("decoder");
                    d.reset();
                    if i > 0
                        && let Ok(prev) = self.read(ti, i - 1)
                    {
                        let _ = d.decode(&prev, 0);
                    }
                }
                let r = st.decoder.as_mut().expect("decoder").decode(&data, self.audio_starts[i].max(0) as u64);
                st.last_decoded = Some(i);
                r.unwrap_or_default()
            }
        };
        let p = Arc::new(decoded);
        st.packets.insert(i, p.clone());
        st.order.push(i);
        if st.order.len() > 4096 {
            let old = st.order.remove(0);
            st.packets.remove(&old);
        }
        Ok(p)
    }
}

/// The Matroska video track as a [`VideoSamples`] table.
struct MkvVideo<'a> {
    src: &'a MkvSource,
    track: usize,
}

impl VideoSamples for MkvVideo<'_> {
    fn count(&self) -> usize {
        self.src.file.tracks[self.track].samples.len()
    }
    fn pts(&self, i: usize) -> i64 {
        self.src.file.tracks[self.track].samples[i].pts
    }
    fn sync_before(&self, i: usize) -> usize {
        self.src.file.tracks[self.track].sync_sample_before(i)
    }
    fn sample_at(&self, t: i64) -> Option<usize> {
        self.src.file.tracks[self.track].sample_at_pts(t)
    }
    fn read(&self, i: usize) -> crate::Result<Vec<u8>> {
        self.src.read(self.track, i)
    }
    fn make_decoder(&self) -> crate::Result<Box<dyn VideoDecoder>> {
        match &self.src.ventry {
            Some(e) => make_video_decoder(e),
            None => Err(CodecError::Unsupported(format!("no decoder for {} video", codec_label(&self.src.file.tracks[self.track].codec)))),
        }
    }
}

impl MediaSource for MkvSource {
    fn info(&self) -> &MediaInfo {
        &self.info
    }

    fn video_frame(&self, req: FrameRequest) -> Result<Arc<VideoFrame>, MediaError> {
        let ti = self.vtrack.ok_or(MediaError::NoStream("video"))?;
        let target = from_tick(&self.file.tracks[ti], req.time.max(Tick::ZERO));
        Ok(self.video.frame(&MkvVideo { src: self, track: ti }, target)?)
    }

    fn audio(&self, start: i64, frames: usize, sample_rate: u32) -> Result<AudioBuffer, MediaError> {
        let ti = self.atrack.ok_or(MediaError::NoStream("audio"))?;
        let n_packets = self.file.tracks[ti].samples.len();
        let ainfo = self.info.audio.as_ref().ok_or(MediaError::NoStream("audio"))?;
        let src_rate = ainfo.sample_rate;
        let ch = ainfo.channels.max(1) as usize;
        let ratio = src_rate as f64 / sample_rate as f64;
        let s0 = (start as f64 * ratio).floor() as i64;
        let need = (frames as f64 * ratio).ceil() as i64 + 2;
        let mut src: Vec<Vec<f32>> = vec![vec![0.0; need.max(0) as usize]; ch];
        let mut st = self.audio.lock().unwrap_or_else(|e| e.into_inner());
        let mut i = self.audio_starts.partition_point(|&x| x <= s0).saturating_sub(1);
        while i < n_packets {
            let pk_start = self.audio_starts[i];
            if pk_start >= s0 + need {
                break;
            }
            let pk = self.audio_packet(&mut st, i)?;
            for (c, dst) in src.iter_mut().enumerate() {
                let Some(chan) = pk.get(c.min(pk.len().saturating_sub(1))) else { continue };
                for (k, v) in chan.iter().enumerate() {
                    let pos = pk_start + k as i64 - s0;
                    if pos >= 0 && (pos as usize) < dst.len() {
                        dst[pos as usize] = *v;
                    }
                }
            }
            i += 1;
        }
        drop(st);
        let frac = s0 as f64 - start as f64 * ratio;
        let mut out = AudioBuffer::silence(sample_rate, ch, frames);
        for k in 0..frames {
            let pos = k as f64 * ratio - frac;
            let i0 = pos.floor().max(0.0) as usize;
            let f = (pos - i0 as f64) as f32;
            for c in 0..ch {
                let a = src[c].get(i0).copied().unwrap_or(0.0);
                let b = src[c].get(i0 + 1).copied().unwrap_or(a);
                out.channels[c][k] = a + (b - a) * f;
            }
        }
        Ok(out)
    }
}

pub fn opener(name: &str, bytes: Arc<[u8]>) -> Option<Result<SharedSource, MediaError>> {
    if !sniff(&bytes) {
        return None;
    }
    Some(MkvSource::open(name, bytes).map(|s| Arc::new(s) as SharedSource).map_err(Into::into))
}
