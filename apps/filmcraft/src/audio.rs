//! cpal audio output: the playback master clock. Settings ▸ Audio Hardware picks the host
//! ("Device Class"), the output device, the I/O buffer size and the sample rate.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use filmcraft_engine::settings::AudioHardwarePrefs;
use filmcraft_ui_egui::{AudioDevices, AudioOut};

pub struct CpalOut {
    stream: Option<cpal::Stream>,
    played: Arc<AtomicU64>,
    rate: u32,
    hw: AudioHardwarePrefs,
    /// Sequence sample rate for "Attempt to force hardware to document sample rate".
    document_rate: Option<u32>,
}

/// The host named in the settings (empty or unknown = the default host).
fn host(name: &str) -> cpal::Host {
    if !name.is_empty()
        && let Some(id) = cpal::available_hosts().into_iter().find(|h| h.name() == name)
        && let Ok(h) = cpal::host_from_id(id)
    {
        return h;
    }
    cpal::default_host()
}

/// The output device named in the settings (empty or missing = the host's default).
fn output_device(h: &cpal::Host, name: &str) -> Option<cpal::Device> {
    if !name.is_empty()
        && let Ok(mut devs) = h.output_devices()
        && let Some(d) = devs.find(|d| d.name().is_ok_and(|n| n == name))
    {
        return Some(d);
    }
    h.default_output_device()
}

impl CpalOut {
    pub fn new() -> Option<Self> {
        let host = cpal::default_host();
        let dev = host.default_output_device()?;
        let cfg = dev.default_output_config().ok()?;
        Some(Self { stream: None, played: Arc::new(AtomicU64::new(0)), rate: cfg.sample_rate().0, hw: AudioHardwarePrefs::default(), document_rate: None })
    }

    /// The stream configuration the settings ask for, falling back to the device default.
    fn config(&self, dev: &cpal::Device) -> Result<cpal::SupportedStreamConfig, String> {
        let default = dev.default_output_config().map_err(|e| e.to_string())?;
        let want = if self.hw.force_document_rate { self.document_rate.unwrap_or(self.hw.sample_rate) } else { self.hw.sample_rate };
        let exact = dev.supported_output_configs().ok().and_then(|mut it| {
            it.find(|c| {
                c.sample_format() == cpal::SampleFormat::F32
                    && c.channels() == default.channels()
                    && (c.min_sample_rate().0..=c.max_sample_rate().0).contains(&want)
            })
            .and_then(|c| c.try_with_sample_rate(cpal::SampleRate(want)))
        });
        Ok(exact.unwrap_or(default))
    }
}

impl AudioOut for CpalOut {
    fn start(&mut self, mut fill: Box<dyn FnMut(&mut [f32], usize) + Send>) -> Result<u32, String> {
        self.stop();
        let host = host(&self.hw.device_class);
        let dev = output_device(&host, &self.hw.default_output).ok_or("no output device")?;
        let cfg = self.config(&dev)?;
        let channels = cfg.channels() as usize;
        let mut config: cpal::StreamConfig = cfg.clone().into();
        if let cpal::SupportedBufferSize::Range { min, max } = cfg.buffer_size()
            && (*min..=*max).contains(&self.hw.buffer_size)
        {
            config.buffer_size = cpal::BufferSize::Fixed(self.hw.buffer_size);
        }
        self.rate = cfg.sample_rate().0;
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
    fn devices(&self) -> AudioDevices {
        let h = host(&self.hw.device_class);
        let outputs = h.output_devices().map(|d| d.filter_map(|x| x.name().ok()).collect()).unwrap_or_default();
        let inputs = h.input_devices().map(|d| d.filter_map(|x| x.name().ok()).collect()).unwrap_or_default();
        let output_channels = output_device(&h, &self.hw.default_output).and_then(|d| self.config(&d).ok()).map(|c| c.channels()).unwrap_or(0);
        AudioDevices { hosts: cpal::available_hosts().iter().map(|h| h.name().to_string()).collect(), inputs, outputs, output_channels }
    }
    fn configure(&mut self, hw: &AudioHardwarePrefs, document_rate: Option<u32>) {
        self.hw = hw.clone();
        self.document_rate = document_rate;
        // the rate playback mixes at must be known before `start`
        let host = host(&self.hw.device_class);
        if let Some(dev) = output_device(&host, &self.hw.default_output)
            && let Ok(cfg) = self.config(&dev)
        {
            self.rate = cfg.sample_rate().0;
        }
    }
}
