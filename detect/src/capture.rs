//! Process-loopback audio capture (Windows 10 2004+): only the target process tree's audio.
//! Discord voice, music players, etc. are never captured.

use lootline_core::dsp::{downmix, SAMPLE_RATE};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::SyncSender;
use std::sync::Arc;
use std::thread::JoinHandle;
use wasapi::{initialize_mta, AudioClient, Direction, SampleType, StreamMode, WaveFormat};

pub enum CapMsg {
    /// Mono 48 kHz f32 samples.
    Audio(Vec<f32>),
    /// Capture stopped (process exited, device error, or stop requested).
    Ended(String),
}

pub struct Capture {
    pub stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Capture {
    pub fn start(pid: u32, tx: SyncSender<CapMsg>) -> Capture {
        let stop = Arc::new(AtomicBool::new(false));
        let s = stop.clone();
        let handle = std::thread::Builder::new()
            .name("lootline-capture".into())
            .spawn(move || {
                let r = capture_loop(pid, &tx, &s);
                let _ = tx.send(CapMsg::Ended(match r {
                    Ok(()) => "stopped".to_string(),
                    Err(e) => format!("{e}"),
                }));
            })
            .ok();
        Capture { stop, handle }
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn capture_loop(pid: u32, tx: &SyncSender<CapMsg>, stop: &AtomicBool) -> anyhow::Result<()> {
    let _ = initialize_mta();
    let channels = 2usize;
    let fmt = WaveFormat::new(32, 32, SampleType::Float, SAMPLE_RATE as usize, channels, None);
    let mut client = AudioClient::new_application_loopback_client(pid, true)
        .map_err(|e| anyhow::anyhow!("process loopback unavailable (needs Windows 10 2004+): {e}"))?;
    let mode = StreamMode::EventsShared { autoconvert: true, buffer_duration_hns: 0 };
    client
        .initialize_client(&fmt, Direction::Capture, mode, None)
        .map_err(|e| anyhow::anyhow!("initialize capture: {e}"))?;
    let event = client.set_get_eventhandle().map_err(|e| anyhow::anyhow!("event handle: {e}"))?;
    let cap = client.get_audiocaptureclient().map_err(|e| anyhow::anyhow!("capture client: {e}"))?;
    client.start_stream().map_err(|e| anyhow::anyhow!("start: {e}"))?;

    let mut bytes: VecDeque<u8> = VecDeque::new();
    let mut inter: Vec<f32> = Vec::new();
    let mut mono: Vec<f32> = Vec::new();
    let mut idle_timeouts = 0u32;
    while !stop.load(Ordering::Relaxed) {
        // The stream may deliver no events while the game is silent, so a timeout is normal.
        match event.wait_for_event(250) {
            Ok(()) => idle_timeouts = 0,
            Err(_) => idle_timeouts += 1,
        }
        loop {
            let frames = cap.get_next_packet_size().map_err(|e| anyhow::anyhow!("packet size: {e}"))?.unwrap_or(0);
            if frames == 0 {
                break;
            }
            bytes.clear();
            cap.read_from_device_to_deque(&mut bytes).map_err(|e| anyhow::anyhow!("read: {e}"))?;
            let (a, b) = bytes.as_slices();
            inter.clear();
            for chunk in a.chunks_exact(4).chain(b.chunks_exact(4)) {
                inter.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
            }
            mono.clear();
            downmix(&inter, channels, &mut mono);
            // Drop audio rather than block if the consumer falls behind.
            if tx.try_send(CapMsg::Audio(mono.clone())).is_err() && stop.load(Ordering::Relaxed) {
                break;
            }
        }
        // Many consecutive timeouts is fine (silence); the owner polls process liveness.
        let _ = idle_timeouts;
    }
    let _ = client.stop_stream();
    Ok(())
}
