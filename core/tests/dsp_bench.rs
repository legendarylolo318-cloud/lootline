//! Detection pipeline cost: CPU time as a fraction of real time for the exact per-packet path
//! the detector runs (interleaved stereo -> downmix -> STFT/log-mel -> matcher).
//!
//! `cargo test --release -p lootline-core --test dsp_bench -- --ignored --nocapture`

use lootline_core::dsp::{downmix, trim_recording, Frame, Matcher, MelExtractor, TemplateFile, SAMPLE_RATE};
use std::time::Instant;

fn lcg(seed: &mut u64) -> f32 {
    *seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    ((*seed >> 33) as f32 / (1u64 << 31) as f32) * 2.0 - 1.0
}

fn chime() -> Vec<f32> {
    let n = SAMPLE_RATE as usize / 2;
    let mut phase = 0.0f32;
    (0..n)
        .map(|i| {
            let t = i as f32 / SAMPLE_RATE as f32;
            phase += 2.0 * std::f32::consts::PI * (1000.0 + 6000.0 * t) / SAMPLE_RATE as f32;
            0.5 * phase.sin() * (-t * 5.0).exp() * (1.0 - (-t * 200.0).exp())
        })
        .collect()
}

fn template() -> TemplateFile {
    let mut seed = 1;
    // recorded like the in-app recorder would: the chime over the same background noise
    let mut audio: Vec<f32> = (0..SAMPLE_RATE as usize / 2).map(|_| lcg(&mut seed) * 0.01).collect();
    let c: Vec<f32> = chime().into_iter().map(|v| v + lcg(&mut seed) * 0.01).collect();
    audio.extend(c);
    audio.extend((0..SAMPLE_RATE as usize / 2).map(|_| lcg(&mut seed) * 0.01));
    let mut ex = MelExtractor::new();
    let mut frames: Vec<Frame> = Vec::new();
    ex.push(&audio, &mut frames);
    TemplateFile::from_frames("chest", "bench", &trim_recording(&frames).unwrap())
}

#[test]
#[ignore]
fn realtime_cost_of_detection() {
    let secs = 60usize;
    let packet = SAMPLE_RATE as usize / 100; // WASAPI delivers ~10 ms packets
                                             // stereo game audio: noise bed with a chime 1 s into every 5 s
    let mut seed = 7;
    let ch = chime();
    let mut stereo = Vec::with_capacity(SAMPLE_RATE as usize * secs * 2);
    for i in 0..SAMPLE_RATE as usize * secs {
        let s = lcg(&mut seed) * 0.01
            + ch.get((i % (SAMPLE_RATE as usize * 5)).wrapping_sub(SAMPLE_RATE as usize)).copied().unwrap_or(0.0);
        stereo.push(s);
        stereo.push(s * 0.9);
    }
    for templates in [1usize, 4] {
        let t = template();
        let ts: Vec<TemplateFile> = (0..templates).map(|_| t.clone()).collect();
        let mut matcher = Matcher::new(&ts, 0.71);
        let mut ex = MelExtractor::new();
        let mut mono = Vec::with_capacity(packet);
        let mut frames: Vec<Frame> = Vec::with_capacity(8);
        let mut hits = 0;
        let mut best = 0.0f32;
        let start = Instant::now();
        for p in stereo.chunks(packet * 2) {
            mono.clear();
            downmix(p, 2, &mut mono);
            frames.clear();
            ex.push(&mono, &mut frames);
            for f in frames.drain(..) {
                if matcher.push_frame(f).is_some() {
                    hits += 1;
                }
                best = best.max(matcher.last_score());
            }
        }
        let cpu = start.elapsed().as_secs_f64();
        println!(
            "templates={templates}: {:.3} ms CPU per second of audio = {:.3}% of one core ({hits} detections in {secs}s, best score {best:.3})",
            cpu * 1000.0 / secs as f64,
            100.0 * cpu / secs as f64
        );
        assert_eq!(hits, secs / 5, "pipeline must still detect every chime");
    }
}
