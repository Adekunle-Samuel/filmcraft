//! cpal audio input for voice-over recording ([`filmcraft_engine::voiceover::AudioInput`]).
//!
//! The stream lives on its own thread (cpal streams are not `Send` on every backend); the
//! callback de-interleaves into a shared planar buffer that the engine drains when recording stops.

use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use filmcraft_engine::voiceover::{AudioInput, InputFormat};

pub struct CpalIn {
    /// Host name (Settings ▸ Audio Hardware ▸ Device Class; empty = default host).
    host: String,
    buf: Arc<Mutex<Vec<Vec<f32>>>>,
    stop: Option<mpsc::Sender<()>>,
}

impl CpalIn {
    pub fn new(host: &str) -> Self {
        Self { host: host.to_string(), buf: Arc::default(), stop: None }
    }
}

fn host(name: &str) -> cpal::Host {
    if !name.is_empty()
        && let Some(id) = cpal::available_hosts().into_iter().find(|h| h.name() == name)
        && let Ok(h) = cpal::host_from_id(id)
    {
        return h;
    }
    cpal::default_host()
}

fn device(h: &cpal::Host, name: &str) -> Option<cpal::Device> {
    if !name.is_empty()
        && let Ok(mut devs) = h.input_devices()
        && let Some(d) = devs.find(|d| d.name().is_ok_and(|n| n == name))
    {
        return Some(d);
    }
    h.default_input_device()
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl AudioInput for CpalIn {
    fn devices(&self) -> Vec<String> {
        host(&self.host).input_devices().map(|d| d.filter_map(|x| x.name().ok()).collect()).unwrap_or_default()
    }
    fn channels(&self, name: &str) -> u16 {
        device(&host(&self.host), name).and_then(|d| d.default_input_config().ok()).map(|c| c.channels()).unwrap_or(0)
    }
    fn start(&mut self, name: &str, sample_rate: u32) -> Result<InputFormat, String> {
        self.stop();
        lock(&self.buf).clear();
        let (host_name, name) = (self.host.clone(), name.to_string());
        let buf = self.buf.clone();
        let (fmt_tx, fmt_rx) = mpsc::channel::<Result<InputFormat, String>>();
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        std::thread::Builder::new()
            .name("voice-over input".into())
            .spawn(move || {
                let open = || -> Result<(cpal::Stream, InputFormat), String> {
                    let h = host(&host_name);
                    let dev = device(&h, &name).ok_or("no input device")?;
                    let default = dev.default_input_config().map_err(|e| e.to_string())?;
                    // the sequence rate when the device offers it in f32, else the device default
                    let cfg = dev
                        .supported_input_configs()
                        .ok()
                        .and_then(|mut it| {
                            it.find(|c| c.sample_format() == cpal::SampleFormat::F32 && (c.min_sample_rate().0..=c.max_sample_rate().0).contains(&sample_rate))
                                .and_then(|c| c.try_with_sample_rate(cpal::SampleRate(sample_rate)))
                        })
                        .unwrap_or(default);
                    if cfg.sample_format() != cpal::SampleFormat::F32 {
                        return Err(format!("unsupported input sample format {:?}", cfg.sample_format()));
                    }
                    let ch = cfg.channels() as usize;
                    let fmt = InputFormat { sample_rate: cfg.sample_rate().0, channels: cfg.channels() };
                    let stream = dev
                        .build_input_stream(
                            &cfg.into(),
                            move |data: &[f32], _| {
                                let mut b = lock(&buf);
                                if b.len() != ch {
                                    *b = vec![Vec::new(); ch];
                                }
                                for frame in data.chunks_exact(ch.max(1)) {
                                    for (c, x) in frame.iter().enumerate() {
                                        b[c].push(*x);
                                    }
                                }
                            },
                            |e| eprintln!("filmcraft: audio input error: {e}"),
                            None,
                        )
                        .map_err(|e| e.to_string())?;
                    stream.play().map_err(|e| e.to_string())?;
                    Ok((stream, fmt))
                };
                match open() {
                    Ok((stream, fmt)) => {
                        let _ = fmt_tx.send(Ok(fmt));
                        // keep the stream alive until stop (or the sender is dropped)
                        let _ = stop_rx.recv();
                        drop(stream);
                    }
                    Err(e) => {
                        let _ = fmt_tx.send(Err(e));
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        let fmt = fmt_rx.recv().map_err(|e| e.to_string())??;
        self.stop = Some(stop_tx);
        Ok(fmt)
    }
    fn read(&mut self, frames: usize) -> Vec<Vec<f32>> {
        let mut b = lock(&self.buf);
        b.iter_mut()
            .map(|c| {
                let n = frames.min(c.len());
                c.drain(..n).collect()
            })
            .collect()
    }
    fn discard(&mut self) {
        lock(&self.buf).iter_mut().for_each(Vec::clear);
    }
    fn stop(&mut self) {
        if let Some(tx) = self.stop.take() {
            let _ = tx.send(());
        }
    }
}
