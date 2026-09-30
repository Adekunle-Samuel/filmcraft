//! Minimal WAV (RIFF/RF64 PCM 8/16/24/32-bit int, 32/64-bit float) source. The full `riff` crate
//! (BWF metadata, AIFF) will replace this.

use std::sync::Arc;

use filmcraft_frame::{AudioBuffer, VideoFrame};
use filmcraft_time::Tick;

use crate::{AudioStreamInfo, FrameRequest, MediaError, MediaInfo, MediaKind, MediaSource, Result};

pub fn sniff(b: &[u8]) -> bool {
    b.len() >= 12 && (&b[0..4] == b"RIFF" || &b[0..4] == b"RF64") && &b[8..12] == b"WAVE"
}

pub struct WavSource {
    info: MediaInfo,
    bytes: Arc<[u8]>,
    data_off: usize,
    data_len: usize,
    channels: usize,
    bits: u16,
    float: bool,
    rate: u32,
}

fn le16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn le32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

impl WavSource {
    pub fn parse(name: &str, bytes: Arc<[u8]>) -> Result<Self> {
        let b = &bytes[..];
        let mut pos = 12;
        let mut fmt = None;
        let mut data = None;
        while pos + 8 <= b.len() {
            let id = &b[pos..pos + 4];
            let mut len = le32(b, pos + 4) as usize;
            let body = pos + 8;
            if id == b"data" && (len == 0xFFFF_FFFF || body + len > b.len()) {
                len = b.len() - body;
            }
            if body + len > b.len() {
                break;
            }
            match id {
                b"fmt " if len >= 16 => {
                    let mut tag = le16(b, body);
                    let ch = le16(b, body + 2);
                    let rate = le32(b, body + 4);
                    let bits = le16(b, body + 14);
                    if tag == 0xFFFE && len >= 40 {
                        tag = le16(b, body + 24);
                    }
                    fmt = Some((tag, ch, rate, bits));
                }
                b"data" => data = Some((body, len)),
                _ => {}
            }
            pos = body + len + (len & 1);
        }
        let (tag, ch, rate, bits) = fmt.ok_or_else(|| MediaError::Decode(format!("{name}: missing fmt chunk")))?;
        let (off, len) = data.ok_or_else(|| MediaError::Decode(format!("{name}: missing data chunk")))?;
        let float = tag == 3;
        if !(tag == 1 || float) || ch == 0 || !matches!(bits, 8 | 16 | 24 | 32 | 64) {
            return Err(MediaError::Unsupported(format!("{name}: WAV format tag {tag}, {bits} bits")));
        }
        let frame_bytes = ch as usize * bits as usize / 8;
        let frames = len / frame_bytes;
        let info = MediaInfo {
            name: name.into(),
            kind: MediaKind::AudioOnly,
            duration: Tick::from_units(frames as i64, rate as i64),
            video: None,
            audio: Some(AudioStreamInfo {
                sample_rate: rate,
                channels: ch as u32,
                codec: if float { "PCM float".into() } else { "PCM".into() },
                bits_per_sample: Some(bits as u32),
            }),
            container: "WAV".into(),
            start_timecode: None,
            file_size: Some(b.len() as u64),
        };
        Ok(Self { info, data_off: off, data_len: frames * frame_bytes, channels: ch as usize, bits, float, rate, bytes })
    }

    fn sample(&self, frame: usize, ch: usize) -> f32 {
        let bps = self.bits as usize / 8;
        let o = self.data_off + (frame * self.channels + ch) * bps;
        let b = &self.bytes;
        match (self.bits, self.float) {
            (8, _) => (b[o] as f32 - 128.0) / 128.0,
            (16, _) => i16::from_le_bytes([b[o], b[o + 1]]) as f32 / 32768.0,
            (24, _) => ((i32::from_le_bytes([0, b[o], b[o + 1], b[o + 2]]) >> 8) as f32) / 8_388_608.0,
            (32, false) => i32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]) as f32 / 2_147_483_648.0,
            (32, true) => f32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]),
            (64, true) => f64::from_le_bytes(b[o..o + 8].try_into().unwrap_or([0; 8])) as f32,
            _ => 0.0,
        }
    }
}

impl MediaSource for WavSource {
    fn info(&self) -> &MediaInfo {
        &self.info
    }
    fn video_frame(&self, _req: FrameRequest) -> Result<Arc<VideoFrame>> {
        Err(MediaError::NoStream("video"))
    }
    fn audio(&self, start: i64, frames: usize, sample_rate: u32) -> Result<AudioBuffer> {
        let total = self.data_len / (self.channels * self.bits as usize / 8);
        let mut out = AudioBuffer::silence(sample_rate, self.channels, frames);
        // Linear-interpolating resample when rates differ (the audio crate provides sinc resampling).
        let ratio = self.rate as f64 / sample_rate as f64;
        for i in 0..frames {
            let src = (start + i as i64) as f64 * ratio;
            if src < 0.0 {
                continue;
            }
            let i0 = src.floor() as usize;
            if i0 >= total {
                break;
            }
            let fr = (src - i0 as f64) as f32;
            for c in 0..self.channels {
                let a = self.sample(i0, c);
                let b = if i0 + 1 < total { self.sample(i0 + 1, c) } else { a };
                out.channels[c][i] = a + (b - a) * fr;
            }
        }
        Ok(out)
    }
}

/// Encode interleaved f32 samples as a 16-bit PCM WAV file.
pub fn write_wav16(samples: &[f32], channels: u16, rate: u32) -> Vec<u8> {
    let data_len = samples.len() * 2;
    let mut v = Vec::with_capacity(44 + data_len);
    v.extend_from_slice(b"RIFF");
    v.extend_from_slice(&((36 + data_len) as u32).to_le_bytes());
    v.extend_from_slice(b"WAVEfmt ");
    v.extend_from_slice(&16u32.to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes());
    v.extend_from_slice(&channels.to_le_bytes());
    v.extend_from_slice(&rate.to_le_bytes());
    v.extend_from_slice(&(rate * channels as u32 * 2).to_le_bytes());
    v.extend_from_slice(&(channels * 2).to_le_bytes());
    v.extend_from_slice(&16u16.to_le_bytes());
    v.extend_from_slice(b"data");
    v.extend_from_slice(&(data_len as u32).to_le_bytes());
    for s in samples {
        v.extend_from_slice(&((s.clamp(-1.0, 1.0) * 32767.0).round() as i16).to_le_bytes());
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let s: Vec<f32> = (0..200).map(|i| ((i as f32) * 0.1).sin() * 0.5).collect();
        let bytes = write_wav16(&s, 2, 48000);
        let src = WavSource::parse("t.wav", bytes.into()).unwrap();
        assert_eq!(src.info().audio.as_ref().unwrap().channels, 2);
        let a = src.audio(0, 100, 48000).unwrap();
        assert!((a.channels[0][3] - s[6]).abs() < 1e-4);
        assert!((a.channels[1][3] - s[7]).abs() < 1e-4);
        assert_eq!(src.info().duration, Tick::from_units(100, 48000));
    }
}
