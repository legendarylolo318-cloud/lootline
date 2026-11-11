//! Process-loopback audio capture (Windows 10 2004+): only the target process tree's audio.
//! Discord voice, music players, etc. are never captured.

use lootline_core::dsp::{downmix, SAMPLE_RATE};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
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
    /// Consumed sample buffers go back to the capture thread for reuse, so steady-state
    /// capture allocates nothing per packet.
    free: SyncSender<Vec<f32>>,
}

impl Capture {
    pub fn start(pid: u32, tx: SyncSender<CapMsg>) -> Capture {
        let stop = Arc::new(AtomicBool::new(false));
        let s = stop.clone();
        let (free, free_rx) = sync_channel::<Vec<f32>>(POOL);
        let handle = std::thread::Builder::new()
            .name("lootline-capture".into())
            .spawn(move || {
                let r = capture_loop(pid, &tx, &free_rx, &s);
                // try_send, never send: the owner joins this thread from Drop while still holding
                // the receiver, so a blocking send into a full queue would deadlock both threads.
                let _ = tx.try_send(CapMsg::Ended(match r {
                    Ok(()) => "stopped".to_string(),
                    Err(e) => format!("{e}"),
                }));
            })
            .ok();
        Capture { stop, handle, free }
    }

    /// Hand a consumed `CapMsg::Audio` buffer back for reuse (dropped if the pool is full).
    pub fn recycle(&self, mut buf: Vec<f32>) {
        buf.clear();
        let _ = self.free.try_send(buf);
    }
}

const POOL: usize = 16;

impl Drop for Capture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn capture_loop(pid: u32, tx: &SyncSender<CapMsg>, free: &Receiver<Vec<f32>>, stop: &AtomicBool) -> anyhow::Result<()> {
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
            // The deque's two halves can split in the middle of a sample, so decode from one
            // contiguous slice (no allocation: it rotates in place).
            inter.clear();
            decode_f32le(bytes.make_contiguous(), &mut inter);
            let mut mono = free.try_recv().unwrap_or_else(|_| Vec::with_capacity(inter.len() / channels + 16));
            mono.clear();
            downmix(&inter, channels, &mut mono);
            // Drop audio rather than block if the consumer falls behind.
            if tx.try_send(CapMsg::Audio(mono)).is_err() && stop.load(Ordering::Relaxed) {
                break;
            }
        }
        // Many consecutive timeouts is fine (silence); the owner polls process liveness.
        let _ = idle_timeouts;
    }
    let _ = client.stop_stream();
    Ok(())
}

/// Little-endian f32 samples from raw bytes (a trailing partial sample is ignored).
pub fn decode_f32le(bytes: &[u8], out: &mut Vec<f32>) {
    let (chunks, _) = bytes.as_chunks::<4>();
    out.extend(chunks.iter().map(|c| f32::from_le_bytes(*c)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_across_a_wrapped_deque() {
        let samples = [0.25f32, -1.0, 3.5, 0.0, 1e-3];
        let data: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        // exact-capacity ring holding 3 junk bytes + the first 17 data bytes; dropping the junk
        // and appending the last 3 bytes wraps the ring 17 bytes in (inside the 5th sample)
        let mut v: Vec<u8> = vec![9u8; 3];
        v.extend_from_slice(&data[..17]);
        let mut dq: VecDeque<u8> = VecDeque::from(v.into_boxed_slice().into_vec());
        dq.drain(..3);
        dq.extend(&data[17..]);
        let (a, _) = dq.as_slices();
        assert_ne!(a.len() % 4, 0, "test setup: the split should fall inside a sample");
        let mut out = Vec::new();
        decode_f32le(dq.make_contiguous(), &mut out);
        assert_eq!(out, samples);
    }
}
