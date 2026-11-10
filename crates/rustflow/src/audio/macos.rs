//! macOS: receive the Linux audio stream and play it on the current output.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use futures::StreamExt;
use tracing::{info, warn};

use super::{Audio, CHANNELS, RETRY_AFTER, SAMPLE_RATE, audio_addr};
use crate::config::Config;
use crate::status::Shared;

/// Samples (interleaved stereo, 48 kHz) to collect before playing starts.
const PRIME: usize = ms(40);
/// If more than this piles up (network burst, clock drift), skip ahead to PRIME.
const MAX_BUFFERED: usize = ms(200);

const fn ms(n: usize) -> usize {
    SAMPLE_RATE as usize / 1000 * n * CHANNELS
}

/// Interleaved stereo samples waiting to be played.
#[derive(Default)]
struct Buffer {
    samples: VecDeque<f32>,
    /// True while refilling after running dry, so playback restarts smoothly.
    priming: bool,
}

pub async fn run(cfg: Config, token: String, shared: Shared, audio: Audio) -> ! {
    let buffer = Arc::new(Mutex::new(Buffer { priming: true, ..Default::default() }));
    let out_buffer = buffer.clone();
    std::thread::Builder::new()
        .name("rustflow-audio".into())
        .spawn(move || output_thread(out_buffer))
        .expect("spawn audio thread");

    let label = cfg.peer_label();
    let addr = cfg.peer_addr().map(|a| audio_addr(&a));
    loop {
        if !audio.enabled() || !cfg.audio.enabled {
            shared.set_audio("Audio: off".into());
        } else {
            match &addr {
                Some(Ok(addr)) => {
                    if let Err(e) = receive(addr, &token, &buffer, &audio, &shared, &label).await {
                        tracing::debug!("audio: {e:#}");
                    }
                    shared.set_audio(format!("Audio: waiting for {label}"));
                }
                Some(Err(e)) => shared.set_audio(format!("Audio error: {e}")),
                None => shared.set_audio("Audio: no peer set".into()),
            }
        }
        tokio::time::sleep(RETRY_AFTER).await;
    }
}

async fn receive(
    addr: &str,
    token: &str,
    buffer: &Mutex<Buffer>,
    audio: &Audio,
    shared: &Shared,
    label: &str,
) -> Result<()> {
    let mut conn = crate::net::side_connect(addr, token).await?;
    info!("audio: connected to {label}");
    shared.set_audio(format!("Audio: playing sound from {label}"));
    while let Some(frame) = conn.next().await {
        if !audio.enabled() {
            break;
        }
        let frame = frame.context("audio stream broke")?;
        let mut buf = buffer.lock().unwrap();
        buf.samples.extend(frame.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0));
        if buf.samples.len() > MAX_BUFFERED {
            let excess = buf.samples.len() - PRIME;
            buf.samples.drain(..excess);
        }
    }
    buffer.lock().unwrap().samples.clear();
    Ok(())
}

/// Keeps an output stream open on the current default device, reopening it
/// when the default changes (headphones plugged in or out).
/// Frames written by the output callback, and how many carried sound.
static FRAMES_OUT: AtomicU64 = AtomicU64::new(0);
static FRAMES_LOUD: AtomicU64 = AtomicU64::new(0);

fn output_thread(buffer: Arc<Mutex<Buffer>>) {
    loop {
        match open_output(buffer.clone()) {
            Ok((stream, device_name, failed)) => {
                info!("audio: playing on {device_name}");
                loop {
                    std::thread::sleep(Duration::from_secs(1));
                    let loud = FRAMES_LOUD.swap(0, Ordering::Relaxed);
                    let total = FRAMES_OUT.swap(0, Ordering::Relaxed);
                    if loud > 0 {
                        tracing::trace!(total, loud, "audio: output frames in the last second");
                    } else if total == 0 {
                        warn!("audio: output callback is not running");
                    }
                    let current = cpal::default_host().default_output_device().map(|d| d.to_string());
                    if failed.load(Ordering::Relaxed) || current.as_deref() != Some(device_name.as_str()) {
                        break;
                    }
                }
                drop(stream);
            }
            Err(e) => {
                warn!("audio: cannot open output: {e:#}");
                std::thread::sleep(Duration::from_secs(3));
            }
        }
    }
}

fn open_output(buffer: Arc<Mutex<Buffer>>) -> Result<(cpal::Stream, String, Arc<AtomicBool>)> {
    let device = cpal::default_host().default_output_device().context("no output device")?;
    let name = device.to_string();
    // Prefer 48 kHz float (no resampling); otherwise take the device default.
    let supported = device
        .supported_output_configs()?
        .find(|c| {
            c.sample_format() == cpal::SampleFormat::F32
                && c.channels() >= 2
                && (c.min_sample_rate()..=c.max_sample_rate()).contains(&SAMPLE_RATE)
        })
        .map(|c| c.with_sample_rate(SAMPLE_RATE))
        .map_or_else(|| device.default_output_config(), Ok)?;
    let config: cpal::StreamConfig = supported.config();
    info!(rate = config.sample_rate, channels = config.channels, format = ?supported.sample_format(), "audio: output format");
    let device_channels = config.channels as usize;
    let mut resampler = Resampler::new(SAMPLE_RATE, config.sample_rate);

    let failed = Arc::new(AtomicBool::new(false));
    let failed_cb = failed.clone();
    let stream = device.build_output_stream(
        config,
        move |out: &mut [f32], _| {
            let mut buf = buffer.lock().unwrap();
            FRAMES_OUT.fetch_add((out.len() / device_channels) as u64, Ordering::Relaxed);
            for frame in out.chunks_exact_mut(device_channels) {
                let (l, r) = resampler.next(&mut buf);
                if l != 0.0 || r != 0.0 {
                    FRAMES_LOUD.fetch_add(1, Ordering::Relaxed);
                }
                frame[0] = l;
                frame[1] = r;
                for extra in &mut frame[2..] {
                    *extra = 0.0;
                }
            }
        },
        move |e| {
            warn!("audio output error: {e}");
            failed_cb.store(true, Ordering::Relaxed);
        },
        None,
    )?;
    stream.play()?;
    Ok((stream, name, failed))
}

/// Linear resampling from the stream rate to the device rate.
struct Resampler {
    step: f64,
    pos: f64,
    prev: (f32, f32),
    next: (f32, f32),
}

impl Resampler {
    fn new(from: u32, to: u32) -> Self {
        Self { step: from as f64 / to as f64, pos: 1.0, prev: (0.0, 0.0), next: (0.0, 0.0) }
    }

    /// One output frame; silence while the buffer is (re)filling.
    fn next(&mut self, buf: &mut Buffer) -> (f32, f32) {
        if buf.priming {
            if buf.samples.len() < PRIME {
                return (0.0, 0.0);
            }
            buf.priming = false;
        }
        while self.pos >= 1.0 {
            let (Some(l), Some(r)) = (buf.samples.pop_front(), buf.samples.pop_front()) else {
                buf.priming = true;
                self.prev = (0.0, 0.0);
                self.next = (0.0, 0.0);
                return (0.0, 0.0);
            };
            self.prev = self.next;
            self.next = (l, r);
            self.pos -= 1.0;
        }
        let t = self.pos as f32;
        self.pos += self.step;
        (self.prev.0 + (self.next.0 - self.prev.0) * t, self.prev.1 + (self.next.1 - self.prev.1) * t)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filled(n: usize) -> Buffer {
        let mut b = Buffer { priming: false, ..Default::default() };
        for i in 0..n {
            b.samples.push_back(i as f32);
            b.samples.push_back(-(i as f32));
        }
        b
    }

    #[test]
    fn same_rate_passes_samples_through() {
        let mut buf = filled(4);
        let mut r = Resampler::new(48_000, 48_000);
        let out: Vec<_> = (0..4).map(|_| r.next(&mut buf)).collect();
        // One frame of delay from interpolation.
        assert_eq!(out[1], (0.0, -0.0));
        assert_eq!(out[2], (1.0, -1.0));
        assert_eq!(out[3], (2.0, -2.0));
    }

    #[test]
    fn runs_dry_into_silence_and_reprimes() {
        let mut buf = filled(1);
        let mut r = Resampler::new(48_000, 48_000);
        r.next(&mut buf);
        assert_eq!(r.next(&mut buf), (0.0, 0.0));
        assert!(buf.priming);
    }
}
