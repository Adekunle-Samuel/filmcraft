//! cpal audio output: the playback master clock.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use filmcraft_ui_egui::AudioOut;

pub struct CpalOut {
    stream: Option<cpal::Stream>,
    played: Arc<AtomicU64>,
    rate: u32,
}

impl CpalOut {
    pub fn new() -> Option<Self> {
        let host = cpal::default_host();
        let dev = host.default_output_device()?;
        let cfg = dev.default_output_config().ok()?;
        Some(Self { stream: None, played: Arc::new(AtomicU64::new(0)), rate: cfg.sample_rate().0 })
    }
}

impl AudioOut for CpalOut {
    fn start(&mut self, mut fill: Box<dyn FnMut(&mut [f32], usize) + Send>) -> Result<u32, String> {
        self.stop();
        let host = cpal::default_host();
        let dev = host.default_output_device().ok_or("no output device")?;
        let cfg = dev.default_output_config().map_err(|e| e.to_string())?;
        let channels = cfg.channels() as usize;
        let config: cpal::StreamConfig = cfg.clone().into();
        self.played.store(0, Ordering::SeqCst);
        let played = self.played.clone();
        let err = |e| eprintln!("filmcraft: audio stream error: {e}");
        let stream = match cfg.sample_format() {
            cpal::SampleFormat::F32 => dev.build_output_stream(
                &config,
                move |buf: &mut [f32], _| {
                    fill(buf, channels);
                    played.fetch_add((buf.len() / channels) as u64, Ordering::SeqCst);
                },
                err,
                None,
            ),
            other => return Err(format!("unsupported sample format {other:?}")),
        }
        .map_err(|e| e.to_string())?;
        stream.play().map_err(|e| e.to_string())?;
        self.stream = Some(stream);
        Ok(self.rate)
    }
    fn stop(&mut self) {
        self.stream = None;
    }
    fn sample_rate(&self) -> u32 {
        self.rate
    }
    fn played_frames(&self) -> Option<u64> {
        self.stream.as_ref().map(|_| self.played.load(Ordering::SeqCst))
    }
}
