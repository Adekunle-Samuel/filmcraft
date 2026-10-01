//! Audio decoding: PCM directly; compressed codecs through symphonia (bootstrap, MPL-2.0 unmodified).

use std::sync::Arc;

use filmcraft_frame::{AudioBuffer, VideoFrame};
use filmcraft_media::{AudioStreamInfo, FrameRequest, MediaError, MediaInfo, MediaKind, MediaSource, SharedSource};
use filmcraft_time::Tick;
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{CODEC_TYPE_AAC, CODEC_TYPE_ALAC, CODEC_TYPE_FLAC, CODEC_TYPE_MP3, CodecParameters, CodecType, Decoder, DecoderOptions};
use symphonia::core::formats::{FormatOptions, Packet};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

use crate::{CodecError, Result};

enum Inner {
    /// Our own AAC-LC decoder.
    Aac { dec: Box<filmcraft_aac::Decoder>, asc: Vec<u8> },
    /// Our own Opus decoder (always 48 kHz output; pre-skip is left to container timestamps).
    /// `order` maps output channel → decoded channel (Vorbis → WAV/SMPTE order for surround).
    Opus { dec: Box<filmcraft_opus::Decoder>, order: Option<&'static [usize]> },
    /// Bootstrap decoders (MP3, ALAC, FLAC, HE-AAC…) via symphonia.
    Symphonia(Box<dyn Decoder>),
}

/// Opus always decodes at 48 kHz (RFC 7845 §5.1: the input rate in the header is informational).
pub const OPUS_RATE: u32 = 48_000;

/// Opus pre-roll decoded before a random-access target, in 48 kHz frames: 320 ms.
///
/// The containers' 80 ms (RFC 7845 §4.6, Matroska `SeekPreRoll`) is enough to sound right, but the
/// CELT energy predictor converges only ~24 dB per 80 ms: measured against a continuous decode, a
/// cold start 80 ms early is ~26 dB off, 320 ms early ~89 dB (float-exact). Seeking must give the
/// same samples as playback (render caches, scrubbing), so we pay the extra few packets.
pub const OPUS_PRE_ROLL: u32 = 4 * 3840;

/// Channel order for Vorbis-ordered surround (Opus mapping family 1, RFC 7845 §5.1.1.2): output
/// channel → decoded channel, giving the WAV/SMPTE order (L R C LFE back… side…) used elsewhere.
fn vorbis_to_wav_order(channels: usize) -> Option<&'static [usize]> {
    Some(match channels {
        3 => &[0, 2, 1],
        5 => &[0, 2, 1, 3, 4],
        6 => &[0, 2, 1, 5, 3, 4],
        7 => &[0, 2, 1, 6, 5, 3, 4],
        8 => &[0, 2, 1, 7, 5, 6, 3, 4],
        _ => return None,
    })
}

/// Duration of an Opus packet in 48 kHz samples from its TOC and frame count (RFC 6716 §3.1),
/// without parsing frame lengths. For a multistream packet this is the first stream's TOC, which
/// every stream shares. `None` for an empty or invalid (> 120 ms) packet.
pub fn opus_packet_samples(data: &[u8]) -> Option<usize> {
    let toc = filmcraft_opus::Toc::parse(*data.first()?);
    let frames = match data[0] & 3 {
        0 => 1,
        1 | 2 => 2,
        _ => (*data.get(1)? & 0x3F) as usize,
    };
    let n = toc.frame_samples_48k() * frames;
    (n > 0 && n <= 5760).then_some(n)
}

/// A packet decoder producing planar f32.
pub struct PacketDecoder {
    inner: Inner,
    pub channels: usize,
}

impl PacketDecoder {
    pub fn new(codec: CodecType, sample_rate: u32, extra: Option<Vec<u8>>) -> Result<Self> {
        let mut p = CodecParameters::new();
        p.for_codec(codec).with_sample_rate(sample_rate);
        if let Some(x) = extra {
            p.with_extra_data(x.into_boxed_slice());
        }
        let dec = symphonia::default::get_codecs().make(&p, &DecoderOptions::default()).map_err(|e| CodecError::Unsupported(e.to_string()))?;
        Ok(Self { inner: Inner::Symphonia(dec), channels: 0 })
    }
    /// AAC: our decoder for AAC-LC; symphonia for other object types (HE-AAC…).
    pub fn aac(asc: &[u8], sample_rate: u32) -> Result<Self> {
        let lc = asc.first().map(|b| b >> 3) == Some(2);
        if lc && let Ok(dec) = filmcraft_aac::Decoder::new(asc) {
            return Ok(Self { inner: Inner::Aac { dec: Box::new(dec), asc: asc.to_vec() }, channels: 0 });
        }
        Self::new(CODEC_TYPE_AAC, sample_rate, Some(asc.to_vec()))
    }
    /// Opus from an `OpusHead` (Matroska `CodecPrivate`, Ogg) or a `dOps` box payload. Output is
    /// 48 kHz; pre-skip is *not* trimmed here (Matroska `CodecDelay` / MP4 edit lists do that).
    pub fn opus(head: filmcraft_opus::OpusHead) -> Result<Self> {
        let dec = filmcraft_opus::Decoder::from_head(head, OPUS_RATE).map_err(|e| CodecError::Unsupported(format!("Opus: {e}")))?;
        let channels = dec.channels();
        let order = if dec.head().mapping_family == 1 { vorbis_to_wav_order(channels) } else { None };
        Ok(Self { inner: Inner::Opus { dec: Box::new(dec), order }, channels })
    }
    /// Whether this decodes Opus (which needs [`OPUS_PRE_ROLL`] of pre-roll after a seek).
    pub fn is_opus(&self) -> bool {
        matches!(self.inner, Inner::Opus { .. })
    }
    pub fn for_isobmff(c: &filmcraft_isobmff::CodecConfig, rate: u32) -> Result<Self> {
        use filmcraft_isobmff::CodecConfig as C;
        match c {
            C::Aac(a) => Self::aac(&a.asc, if a.sample_rate > 0 { a.sample_rate } else { rate }),
            C::Mp3 => Self::new(CODEC_TYPE_MP3, rate, None),
            C::Alac { cookie } => Self::new(CODEC_TYPE_ALAC, rate, Some(cookie.clone())),
            C::Flac(_) => Self::new(CODEC_TYPE_FLAC, rate, None),
            C::Opus(o) => Self::opus(filmcraft_opus::OpusHead::from_dops(&o.to_bytes()).map_err(|e| CodecError::Unsupported(format!("Opus: {e}")))?),
            other => Err(CodecError::Unsupported(format!("{} audio", other.name()))),
        }
    }
    /// Decode one packet into planar channels.
    pub fn decode(&mut self, data: &[u8], ts: u64) -> Result<Vec<Vec<f32>>> {
        let dec = match &mut self.inner {
            Inner::Aac { dec, .. } => {
                let out = dec.decode(data).map_err(|e| CodecError::Decode(e.to_string()))?;
                self.channels = out.len();
                return Ok(out);
            }
            Inner::Opus { dec, order } => {
                // A corrupt packet is concealed like a lost one (keeps timing and decoder state).
                let mut out = match dec.decode(Some(data)) {
                    Ok(o) => o,
                    Err(_) => dec.decode(None).map_err(|e| CodecError::Decode(e.to_string()))?,
                };
                if let Some(order) = order {
                    out = order.iter().map(|&c| std::mem::take(&mut out[c])).collect();
                }
                self.channels = out.len();
                return Ok(out);
            }
            Inner::Symphonia(d) => d,
        };
        let pkt = Packet::new_from_slice(0, ts, 0, data);
        let buf = dec.decode(&pkt).map_err(|e| CodecError::Decode(e.to_string()))?;
        let spec = *buf.spec();
        let ch = spec.channels.count().max(1);
        self.channels = ch;
        let mut sb = SampleBuffer::<f32>::new(buf.capacity() as u64, spec);
        sb.copy_interleaved_ref(buf);
        let s = sb.samples();
        let n = s.len() / ch;
        let mut out = vec![Vec::with_capacity(n); ch];
        for i in 0..n {
            for (c, o) in out.iter_mut().enumerate() {
                o.push(s[i * ch + c]);
            }
        }
        Ok(out)
    }
    pub fn reset(&mut self) {
        match &mut self.inner {
            Inner::Aac { dec, asc } => {
                if let Ok(d) = filmcraft_aac::Decoder::new(asc) {
                    **dec = d;
                }
            }
            Inner::Opus { dec, .. } => dec.reset(),
            Inner::Symphonia(d) => d.reset(),
        }
    }
}

/// Decode interleaved PCM bytes into planar f32.
pub fn decode_pcm(data: &[u8], cfg: &filmcraft_isobmff::PcmConfig) -> Vec<Vec<f32>> {
    let ch = cfg.channels.max(1) as usize;
    let bps = (cfg.bits as usize).div_ceil(8);
    let n = data.len() / (bps * ch);
    let mut out = vec![Vec::with_capacity(n); ch];
    for i in 0..n {
        for (c, o) in out.iter_mut().enumerate() {
            let off = (i * ch + c) * bps;
            let b = &data[off..off + bps];
            let v = match (cfg.bits, cfg.float, cfg.big_endian) {
                (32, true, false) => f32::from_le_bytes([b[0], b[1], b[2], b[3]]),
                (32, true, true) => f32::from_be_bytes([b[0], b[1], b[2], b[3]]),
                (64, true, false) => f64::from_le_bytes(b.try_into().unwrap_or([0; 8])) as f32,
                (64, true, true) => f64::from_be_bytes(b.try_into().unwrap_or([0; 8])) as f32,
                (8, _, _) => {
                    if cfg.signed {
                        b[0] as i8 as f32 / 128.0
                    } else {
                        (b[0] as f32 - 128.0) / 128.0
                    }
                }
                (16, _, false) => i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0,
                (16, _, true) => i16::from_be_bytes([b[0], b[1]]) as f32 / 32768.0,
                (24, _, false) => (i32::from_le_bytes([0, b[0], b[1], b[2]]) >> 8) as f32 / 8_388_608.0,
                (24, _, true) => (i32::from_be_bytes([b[0], b[1], b[2], 0]) >> 8) as f32 / 8_388_608.0,
                (32, false, false) => i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f32 / 2_147_483_648.0,
                (32, false, true) => i32::from_be_bytes([b[0], b[1], b[2], b[3]]) as f32 / 2_147_483_648.0,
                _ => 0.0,
            };
            o.push(v);
        }
    }
    out
}

/// Resample-read `frames` from planar source audio at `src_rate` into a buffer at `rate`,
/// starting at output sample `start` (linear interpolation; the audio crate adds sinc later).
pub fn read_resampled(src: &[Vec<f32>], src_rate: u32, start: i64, frames: usize, rate: u32) -> AudioBuffer {
    let ch = src.len().max(1);
    let mut out = AudioBuffer::silence(rate, ch, frames);
    let total = src.first().map_or(0, Vec::len);
    let ratio = src_rate as f64 / rate as f64;
    for i in 0..frames {
        let pos = (start + i as i64) as f64 * ratio;
        if pos < 0.0 {
            continue;
        }
        let i0 = pos.floor() as usize;
        if i0 >= total {
            break;
        }
        let f = (pos - i0 as f64) as f32;
        for (c, s) in src.iter().enumerate() {
            let a = s[i0];
            let b = s.get(i0 + 1).copied().unwrap_or(a);
            out.channels[c][i] = a + (b - a) * f;
        }
    }
    out
}

/// A standalone audio file decoded fully into memory (audio files are small next to video).
pub struct AudioFileSource {
    info: MediaInfo,
    rate: u32,
    samples: Vec<Vec<f32>>,
}

impl AudioFileSource {
    pub fn decode(name: &str, bytes: Arc<[u8]>) -> Result<Self> {
        let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
        let mss = MediaSourceStream::new(Box::new(std::io::Cursor::new(bytes.clone())), Default::default());
        let mut hint = Hint::new();
        hint.with_extension(&ext);
        let probed = symphonia::default::get_probe()
            .format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default())
            .map_err(|e| CodecError::Unsupported(e.to_string()))?;
        let mut format = probed.format;
        let track = format.default_track().ok_or_else(|| CodecError::Unsupported("no audio track".into()))?.clone();
        let rate = track.codec_params.sample_rate.unwrap_or(48_000);
        let mut dec =
            symphonia::default::get_codecs().make(&track.codec_params, &DecoderOptions::default()).map_err(|e| CodecError::Unsupported(e.to_string()))?;
        let mut samples: Vec<Vec<f32>> = Vec::new();
        while let Ok(pkt) = format.next_packet() {
            if pkt.track_id() != track.id {
                continue;
            }
            let Ok(buf) = dec.decode(&pkt) else { continue };
            let spec = *buf.spec();
            let ch = spec.channels.count().max(1);
            if samples.is_empty() {
                samples = vec![Vec::new(); ch];
            }
            let mut sb = SampleBuffer::<f32>::new(buf.capacity() as u64, spec);
            sb.copy_interleaved_ref(buf);
            for (i, v) in sb.samples().iter().enumerate() {
                if let Some(c) = samples.get_mut(i % ch) {
                    c.push(*v);
                }
            }
        }
        if samples.is_empty() {
            return Err(CodecError::Decode("no audio decoded".into()));
        }
        let frames = samples[0].len();
        let codec_name =
            symphonia::default::get_codecs().get_codec(track.codec_params.codec).map(|d| d.short_name.to_uppercase()).unwrap_or_else(|| ext.to_uppercase());
        let info = MediaInfo {
            name: name.to_string(),
            kind: MediaKind::AudioOnly,
            duration: Tick::from_units(frames as i64, rate as i64),
            video: None,
            audio: Some(AudioStreamInfo {
                sample_rate: rate,
                channels: samples.len() as u32,
                codec: codec_name,
                bits_per_sample: track.codec_params.bits_per_sample,
            }),
            container: ext.to_uppercase(),
            start_timecode: None,
            file_size: Some(bytes.len() as u64),
        };
        Ok(Self { info, rate, samples })
    }
}

impl MediaSource for AudioFileSource {
    fn info(&self) -> &MediaInfo {
        &self.info
    }
    fn video_frame(&self, _req: FrameRequest) -> std::result::Result<Arc<VideoFrame>, MediaError> {
        Err(MediaError::NoStream("video"))
    }
    fn audio(&self, start: i64, frames: usize, sample_rate: u32) -> std::result::Result<AudioBuffer, MediaError> {
        Ok(read_resampled(&self.samples, self.rate, start, frames, sample_rate))
    }
}

pub fn opener(name: &str, bytes: Arc<[u8]>) -> Option<std::result::Result<SharedSource, MediaError>> {
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    if !matches!(ext.as_str(), "mp3" | "flac" | "ogg" | "oga" | "aif" | "aiff" | "aifc") {
        return None;
    }
    Some(AudioFileSource::decode(name, bytes).map(|s| Arc::new(s) as SharedSource).map_err(Into::into))
}
