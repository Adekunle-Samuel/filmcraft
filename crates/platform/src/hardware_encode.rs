//! The VideoToolbox H.264 encoder behind `filmcraft_export::VideoEncoder` (macOS; safe code around
//! [`crate::videotoolbox_encode`]).
//!
//! Hardware encoding is opt-in per export (`ExportSettings::hardware_encoding`, `Auto`): its output depends
//! on the machine, while the built-in encoder's is byte-identical everywhere. The factory declines,
//! so the built-in encoder is used, for everything the hardware path does not take: other formats,
//! MXF (Annex B), two-pass VBR, HDR, odd picture sizes, non-square pixels, and any configuration
//! VideoToolbox cannot create a hardware session for. A hardware encoder that fails in the middle of an export reports
//! an error (the export stops); it cannot hand the stream over to another encoder.

use filmcraft_export::{BitrateMode, EncodedPacket, EncoderFrame, ExportError, ExportSettings, Format, H264Pass, H264Profile, HardwareEncoding, VideoEncoder};
use filmcraft_isobmff::{AvcConfig, SampleEntry};
use filmcraft_time::FrameRate;

use crate::videotoolbox_encode::{VtConfig, VtEncoder, VtPacket, VtProfile, VtRate};

/// The hardware encoder's configuration for an export, or why it is not used for it.
pub fn config_for(w: u32, h: u32, rate: FrameRate, s: &ExportSettings) -> Result<VtConfig, String> {
    if s.hardware_encoding != HardwareEncoding::Auto {
        return Err("hardware encoding is off".into());
    }
    if s.format.is_mxf() {
        return Err("MXF carries an Annex B stream".into());
    }
    if s.bitrate_mode == BitrateMode::Vbr2Pass || !matches!(s.h264_pass, H264Pass::Single) {
        return Err("two-pass encoding".into());
    }
    if s.signal.is_hdr() {
        return Err("HDR".into());
    }
    if !w.is_multiple_of(2) || !h.is_multiple_of(2) {
        return Err("odd picture size".into());
    }
    if s.pixel_aspect.is_some_and(|(n, d)| n != d) {
        return Err("non-square pixels".into());
    }
    let timescale = u32::try_from(rate.num).ok().filter(|n| *n > 0).ok_or("invalid frame rate")?;
    let frame_duration = u32::try_from(rate.den).ok().filter(|d| *d > 0).ok_or("invalid frame rate")?;
    let kbps = s.bitrate_kbps.max(100);
    let max_kbps = s.max_bitrate_kbps.filter(|m| *m >= kbps).unwrap_or(kbps.saturating_mul(3) / 2);
    let fps = f64::from(timescale) / f64::from(frame_duration);
    Ok(VtConfig {
        width: w,
        height: h,
        timescale,
        frame_duration,
        keyframe_interval: s.keyframe_distance.filter(|k| *k > 0).unwrap_or_else(|| (fps * 2.0).round().max(1.0) as u32),
        profile: match s.h264_profile {
            H264Profile::Baseline => VtProfile::Baseline,
            H264Profile::Main => VtProfile::Main,
            H264Profile::High => VtProfile::High,
        },
        rate: match s.bitrate_mode {
            BitrateMode::Cbr => VtRate::Cbr { kbps },
            _ => VtRate::Vbr { target_kbps: kbps, max_kbps },
        },
    })
}

/// The encoder factory `register` puts in front of the built-in ones: a hardware H.264 encoder when
/// the export asks for one and the OS takes the configuration, `None` (the built-in encoder) otherwise.
pub fn videotoolbox_encoder_factory(
    format: Format,
    w: u32,
    h: u32,
    rate: FrameRate,
    s: &ExportSettings,
) -> Option<filmcraft_export::Result<Box<dyn VideoEncoder>>> {
    if format != Format::H264 || s.hardware_encoding != HardwareEncoding::Auto {
        return None;
    }
    let config = match config_for(w, h, rate, s) {
        Ok(c) => c,
        Err(why) => {
            log::info!("hardware H.264 encoding declined: {why}");
            return None;
        }
    };
    match VtEncoder::new(config) {
        Ok(vt) => Some(Ok(Box::new(HardwareH264Encoder { vt, y: Vec::new(), u: Vec::new(), v: Vec::new(), decoded: 0 }))),
        Err(why) => {
            log::info!("hardware H.264 encoding declined: {why}");
            None
        }
    }
}

/// [`VtEncoder`] as a `VideoEncoder`: RGBA pictures in, MP4 samples out.
pub struct HardwareH264Encoder {
    vt: VtEncoder,
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
    /// Compressed frames returned so far.
    decoded: i64,
}

impl HardwareH264Encoder {
    /// MP4 samples from compressed frames. Without frame reordering every frame is presented at
    /// its decode time, so composition offsets are 0; a frame whose time is not `n × duration`
    /// after the first (a dropped or reordered frame) is an error, never a file with wrong times.
    fn samples(&mut self, packets: Vec<VtPacket>) -> filmcraft_export::Result<Vec<EncodedPacket>> {
        let duration = self.vt.config().frame_duration;
        let mut out = Vec::with_capacity(packets.len());
        for p in packets {
            let n = self.decoded;
            self.decoded += 1;
            if n.checked_mul(i64::from(duration)) != Some(p.pts) || p.pts != p.dts {
                return Err(ExportError::Encode(format!(
                    "VideoToolbox returned frame {n} at time {} (decode time {}), expected {}",
                    p.pts,
                    p.dts,
                    n.saturating_mul(i64::from(duration))
                )));
            }
            out.push(EncodedPacket { data: p.data, key: p.key, duration, composition_offset: 0 });
        }
        Ok(out)
    }
}

impl VideoEncoder for HardwareH264Encoder {
    fn sample_entry(&self) -> SampleEntry {
        let c = self.vt.config();
        let (sps, pps) = self.vt.parameter_sets().map(|p| (p.sps, p.pps)).unwrap_or_default();
        SampleEntry::avc(AvcConfig::new(sps, pps, 4), c.width.min(u32::from(u16::MAX)) as u16, c.height.min(u32::from(u16::MAX)) as u16)
    }

    fn timescale(&self) -> u32 {
        self.vt.config().timescale
    }

    fn encode(&mut self, f: &EncoderFrame) -> filmcraft_export::Result<Vec<EncodedPacket>> {
        let c = self.vt.config();
        if f.hdr.is_some() {
            return Err(ExportError::Unsupported("hardware H.264 encoding does not take HDR pictures".into()));
        }
        if (f.width, f.height) != (c.width, c.height) {
            return Err(ExportError::Encode(format!("picture is {}x{}, the encoder was created for {}x{}", f.width, f.height, c.width, c.height)));
        }
        let (w, h) = (f.width as usize, f.height as usize);
        if f.rgba.len() < w.saturating_mul(h).saturating_mul(4) {
            return Err(ExportError::Encode("the RGBA picture is smaller than its size".into()));
        }
        filmcraft_export::rgba_to_yuv420_8(f.rgba, w, h, &mut self.y, &mut self.u, &mut self.v);
        let packets = self.vt.encode(f.index, &self.y, &self.u, &self.v).map_err(ExportError::Encode)?;
        // the muxer needs the parameter sets after the first group of pictures
        if f.index == 0 && self.vt.parameter_sets().is_none() {
            return Err(ExportError::Encode("VideoToolbox returned no parameter sets for the first frame".into()));
        }
        self.samples(packets)
    }

    fn flush(&mut self) -> filmcraft_export::Result<Vec<EncodedPacket>> {
        let packets = self.vt.flush().map_err(ExportError::Encode)?;
        self.samples(packets)
    }
}
