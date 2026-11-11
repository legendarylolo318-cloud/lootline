//! Audio fingerprinting: ring buffer -> STFT (1024/512) -> log-mel (40 bands) -> normalised
//! cross-correlation against templates. Pure Rust, platform independent, unit-tested with
//! synthetic audio.

use anyhow::{ensure, Result};
use realfft::{RealFftPlanner, RealToComplex};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::Arc;

pub const SAMPLE_RATE: u32 = 48_000;
pub const WINDOW: usize = 1024;
pub const HOP: usize = 512;
pub const N_MELS: usize = 40;
pub const FRAME_SECS: f32 = HOP as f32 / SAMPLE_RATE as f32;

pub type Frame = [f32; N_MELS];

/// Fixed-capacity ring buffer. Pushing beyond capacity overwrites the oldest samples.
pub struct RingBuffer<T: Copy + Default> {
    buf: Vec<T>,
    head: usize,
    len: usize,
}

impl<T: Copy + Default> RingBuffer<T> {
    pub fn new(capacity: usize) -> Self {
        RingBuffer { buf: vec![T::default(); capacity.max(1)], head: 0, len: 0 }
    }
    pub fn len(&self) -> usize {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    pub fn capacity(&self) -> usize {
        self.buf.len()
    }
    pub fn push_slice(&mut self, s: &[T]) {
        for &v in s {
            let cap = self.buf.len();
            let tail = (self.head + self.len) % cap;
            self.buf[tail] = v;
            if self.len == cap {
                self.head = (self.head + 1) % cap;
            } else {
                self.len += 1;
            }
        }
    }
    /// Copy the oldest `out.len()` items without consuming. Returns false if not enough data.
    pub fn peek(&self, out: &mut [T]) -> bool {
        if out.len() > self.len {
            return false;
        }
        let cap = self.buf.len();
        for (i, o) in out.iter_mut().enumerate() {
            *o = self.buf[(self.head + i) % cap];
        }
        true
    }
    pub fn discard(&mut self, n: usize) {
        let n = n.min(self.len);
        self.head = (self.head + n) % self.buf.len();
        self.len -= n;
    }
}

fn hz_to_mel(f: f32) -> f32 {
    2595.0 * (1.0 + f / 700.0).log10()
}
fn mel_to_hz(m: f32) -> f32 {
    700.0 * (10f32.powf(m / 2595.0) - 1.0)
}

/// Sparse triangular mel filterbank: for each band a list of (fft bin, weight).
fn mel_filterbank() -> Vec<Vec<(usize, f32)>> {
    let bins = WINDOW / 2 + 1;
    let (fmin, fmax) = (80.0f32, 16_000.0f32);
    let (mmin, mmax) = (hz_to_mel(fmin), hz_to_mel(fmax));
    let pts: Vec<f32> =
        (0..N_MELS + 2).map(|i| mel_to_hz(mmin + (mmax - mmin) * i as f32 / (N_MELS + 1) as f32)).collect();
    let bin_hz = SAMPLE_RATE as f32 / WINDOW as f32;
    (0..N_MELS)
        .map(|b| {
            let (lo, mid, hi) = (pts[b], pts[b + 1], pts[b + 2]);
            let mut v = Vec::new();
            for k in 0..bins {
                let f = k as f32 * bin_hz;
                let w = if f > lo && f <= mid {
                    (f - lo) / (mid - lo)
                } else if f > mid && f < hi {
                    (hi - f) / (hi - mid)
                } else {
                    0.0
                };
                if w > 0.0 {
                    v.push((k, w));
                }
            }
            if v.is_empty() {
                // very narrow band: use the nearest bin
                let k = ((mid / bin_hz).round() as usize).min(bins - 1);
                v.push((k, 1.0));
            }
            v
        })
        .collect()
}

/// Streaming mono 48 kHz samples -> log-mel frames (one per 512 samples).
pub struct MelExtractor {
    fft: Arc<dyn RealToComplex<f32>>,
    window: Vec<f32>,
    filters: Vec<Vec<(usize, f32)>>,
    ring: RingBuffer<f32>,
    frame_buf: Vec<f32>,
    spectrum: Vec<realfft::num_complex::Complex<f32>>,
    scratch: Vec<realfft::num_complex::Complex<f32>>,
}

impl Default for MelExtractor {
    fn default() -> Self {
        Self::new()
    }
}

impl MelExtractor {
    pub fn new() -> Self {
        let fft = RealFftPlanner::<f32>::new().plan_fft_forward(WINDOW);
        let window =
            (0..WINDOW).map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / WINDOW as f32).cos()).collect();
        MelExtractor {
            spectrum: fft.make_output_vec(),
            scratch: fft.make_scratch_vec(),
            fft,
            window,
            filters: mel_filterbank(),
            ring: RingBuffer::new(WINDOW * 8),
            frame_buf: vec![0.0; WINDOW],
        }
    }

    /// Feed samples; appends every completed frame to `out`.
    pub fn push(&mut self, mono: &[f32], out: &mut Vec<Frame>) {
        for chunk in mono.chunks(WINDOW * 4) {
            self.ring.push_slice(chunk);
            while self.ring.peek(&mut self.frame_buf) {
                out.push(self.compute());
                self.ring.discard(HOP);
            }
        }
    }

    fn compute(&mut self) -> Frame {
        for (s, w) in self.frame_buf.iter_mut().zip(&self.window) {
            *s *= w;
        }
        // realfft only errors on length mismatch, which cannot happen here.
        let _ = self.fft.process_with_scratch(&mut self.frame_buf, &mut self.spectrum, &mut self.scratch);
        let mut f = [0.0f32; N_MELS];
        for (b, taps) in self.filters.iter().enumerate() {
            let mut e = 0.0f32;
            for &(k, w) in taps {
                e += self.spectrum[k].norm_sqr() * w;
            }
            f[b] = (e + 1e-6).ln();
        }
        f
    }
}

/// Downmix interleaved frames to mono.
pub fn downmix(interleaved: &[f32], channels: usize, out: &mut Vec<f32>) {
    if channels <= 1 {
        out.extend_from_slice(interleaved);
        return;
    }
    let inv = 1.0 / channels as f32;
    out.extend(interleaved.chunks_exact(channels).map(|c| c.iter().sum::<f32>() * inv));
}

// ---------------------------------------------------------------- templates

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TemplateFile {
    pub name: String,
    #[serde(default)]
    pub season: String,
    pub n_mels: usize,
    pub sample_rate: u32,
    pub hop: usize,
    pub frames: Vec<Vec<f32>>,
}

impl TemplateFile {
    pub fn from_frames(name: &str, season: &str, frames: &[Frame]) -> TemplateFile {
        TemplateFile {
            name: name.into(),
            season: season.into(),
            n_mels: N_MELS,
            sample_rate: SAMPLE_RATE,
            hop: HOP,
            // 2 decimals is plenty and keeps files small
            frames: frames.iter().map(|f| f.iter().map(|v| (v * 100.0).round() / 100.0).collect()).collect(),
        }
    }
    pub fn parse(json: &str) -> Result<TemplateFile> {
        let t: TemplateFile = serde_json::from_str(json)?;
        t.validate()?;
        Ok(t)
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(self.n_mels == N_MELS, "template has {} bands, expected {N_MELS}", self.n_mels);
        ensure!(self.sample_rate == SAMPLE_RATE && self.hop == HOP, "template made with different STFT settings");
        ensure!(self.frames.len() >= 6, "template too short");
        ensure!(self.frames.len() <= 400, "template too long");
        ensure!(self.frames.iter().all(|f| f.len() == N_MELS), "ragged template");
        ensure!(
            self.frames.iter().flatten().all(|v| v.is_finite() && v.abs() < 1e4),
            "template contains non-finite or absurd values"
        );
        Ok(())
    }
}

struct Prepared {
    name: String,
    len: usize,
    /// zero-mean, unit-norm flattened frames
    norm: Vec<f32>,
}

fn prepare(t: &TemplateFile) -> Prepared {
    let flat: Vec<f32> = t.frames.iter().flatten().copied().collect();
    let mean = flat.iter().sum::<f32>() / flat.len() as f32;
    let mut v: Vec<f32> = flat.iter().map(|x| x - mean).collect();
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-9);
    v.iter_mut().for_each(|x| *x /= n);
    Prepared { name: t.name.clone(), len: t.frames.len(), norm: v }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Detection {
    pub template: String,
    pub score: f32,
    /// Length of the matched template in seconds (the detection fires at the *end* of the sound).
    pub duration_secs: f32,
}

/// Sliding normalised cross-correlation (Pearson) of the recent log-mel history against every
/// template. Subtracting the global mean makes it invariant to overall volume.
pub struct Matcher {
    templates: Vec<Prepared>,
    history: VecDeque<Frame>,
    max_len: usize,
    pub threshold: f32,
    /// best (score, template) seen since the threshold was first crossed, with frames waited
    pending: Option<(f32, usize, usize)>,
    refractory: usize,
    last_score: f32,
}

const PEAK_HOLD_FRAMES: usize = 4;
/// Windows with less log-spectral variation than this are treated as silence.
const MIN_STD: f32 = 0.35;

impl Matcher {
    pub fn new(templates: &[TemplateFile], threshold: f32) -> Matcher {
        let prepared: Vec<Prepared> = templates.iter().map(prepare).collect();
        let max_len = prepared.iter().map(|p| p.len).max().unwrap_or(1);
        Matcher {
            templates: prepared,
            history: VecDeque::with_capacity(max_len + 1),
            max_len,
            threshold,
            pending: None,
            refractory: 0,
            last_score: 0.0,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.templates.is_empty()
    }

    /// Best score computed on the most recent frame (for UI meters / tuning).
    pub fn last_score(&self) -> f32 {
        self.last_score
    }

    fn score(&self, p: &Prepared) -> f32 {
        let n = self.history.len();
        if n < p.len {
            return 0.0;
        }
        let start = n - p.len;
        let count = (p.len * N_MELS) as f32;
        let mut sum = 0.0f32;
        for f in self.history.iter().skip(start) {
            sum += f.iter().sum::<f32>();
        }
        let mean = sum / count;
        let (mut dot, mut sq) = (0.0f32, 0.0f32);
        let mut i = 0;
        for f in self.history.iter().skip(start) {
            for &v in f.iter() {
                let d = v - mean;
                dot += d * p.norm[i];
                sq += d * d;
                i += 1;
            }
        }
        let std = (sq / count).sqrt();
        if std < MIN_STD {
            return 0.0;
        }
        dot / sq.sqrt().max(1e-9)
    }

    /// Feed one frame; returns a detection when a peak above threshold has been resolved.
    pub fn push_frame(&mut self, f: Frame) -> Option<Detection> {
        self.history.push_back(f);
        while self.history.len() > self.max_len {
            self.history.pop_front();
        }
        if self.refractory > 0 {
            self.refractory -= 1;
            self.last_score = 0.0;
            return None;
        }
        let mut best = (0.0f32, 0usize);
        for (i, p) in self.templates.iter().enumerate() {
            let s = self.score(p);
            if s > best.0 {
                best = (s, i);
            }
        }
        self.last_score = best.0;
        match self.pending {
            None => {
                if best.0 >= self.threshold {
                    self.pending = Some((best.0, best.1, 0));
                }
            }
            Some((bs, bi, waited)) => {
                let (bs, bi) = if best.0 > bs { (best.0, best.1) } else { (bs, bi) };
                if waited + 1 >= PEAK_HOLD_FRAMES {
                    self.pending = None;
                    self.refractory = self.templates[bi].len;
                    return Some(Detection {
                        template: self.templates[bi].name.clone(),
                        score: bs,
                        duration_secs: self.templates[bi].len as f32 * FRAME_SECS,
                    });
                }
                self.pending = Some((bs, bi, waited + 1));
            }
        }
        None
    }
}

/// Longest template kept (~1.3 s); the chest sound is well under a second.
pub const MAX_TEMPLATE_FRAMES: usize = 120;

/// Trim a raw recording (a few seconds of game audio around the sound) to just the sound.
///
/// Game audio is never silent (music, footsteps, wind), so "first frame above the noise floor"
/// lands on background. Instead the template is anchored on the loudest moment (energy smoothed
/// over 3 frames) and grown outwards only while the energy stays clearly above the background
/// level, tolerating dips of up to 3 frames inside the sound, then padded by 2 frames.
pub fn trim_recording(frames: &[Frame]) -> Result<Vec<Frame>> {
    ensure!(frames.len() >= 10, "recording too short");
    // total linear energy (log of the sum), robust to a sound occupying only a few bands
    let raw: Vec<f32> = frames.iter().map(|f| f.iter().map(|v| v.exp()).sum::<f32>().max(1e-9).ln()).collect();
    let n = raw.len();
    let e: Vec<f32> = (0..n)
        .map(|i| {
            let (a, b) = (i.saturating_sub(1), (i + 1).min(n - 1));
            raw[a..=b].iter().sum::<f32>() / (b - a + 1) as f32
        })
        .collect();
    let mut sorted = e.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let floor = sorted[n / 2];
    let (peak_i, peak) =
        e.iter().copied().enumerate().fold((0, f32::MIN), |best, (i, v)| if v > best.1 { (i, v) } else { best });
    ensure!(
        peak - floor > 1.0,
        "no clear sound found in the recording (too quiet, or drowned out by other game audio)"
    );
    let thr = floor + 0.4 * (peak - floor);
    let above = |i: usize| e[i] > thr;
    const GAP: usize = 3;
    // grow left (the onset is close to the peak, so allow only a short lead-in)
    let mut first = peak_i;
    while first > 0 && peak_i - first < 30 && (1..=GAP).any(|d| first >= d && above(first - d)) {
        first -= 1;
    }
    // grow right (the decay can be long)
    let mut last = peak_i;
    while last + 1 < n
        && last - first + 1 < MAX_TEMPLATE_FRAMES - 4
        && (1..=GAP).any(|d| last + d < n && above(last + d))
    {
        last += 1;
    }
    let start = first.saturating_sub(2);
    let end = (last + 3).min(n).min(start + MAX_TEMPLATE_FRAMES);
    ensure!(end - start >= 6, "detected sound is too short (<6 frames)");
    Ok(frames[start..end].to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self) -> f32 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((self.0 >> 33) as f32 / (1u64 << 31) as f32) * 2.0 - 1.0
        }
    }

    fn noise(n: usize, amp: f32, seed: u64) -> Vec<f32> {
        let mut r = Lcg(seed);
        (0..n).map(|_| r.next() * amp).collect()
    }

    /// A chime-like sound: rising chirp with exponential decay, 0.5 s.
    fn chime(gain: f32) -> Vec<f32> {
        let n = SAMPLE_RATE as usize / 2;
        let mut phase = 0.0f32;
        (0..n)
            .map(|i| {
                let t = i as f32 / SAMPLE_RATE as f32;
                let f = 1000.0 + 6000.0 * t;
                phase += 2.0 * std::f32::consts::PI * f / SAMPLE_RATE as f32;
                gain * phase.sin() * (-t * 5.0).exp() * (1.0 - (-t * 200.0).exp())
            })
            .collect()
    }

    fn other_sound() -> Vec<f32> {
        let n = SAMPLE_RATE as usize / 2;
        (0..n)
            .map(|i| {
                let t = i as f32 / SAMPLE_RATE as f32;
                0.5 * (2.0 * std::f32::consts::PI * (6000.0 - 4000.0 * t) * t).sin() * (-t * 4.0).exp()
            })
            .collect()
    }

    fn frames_of(samples: &[f32]) -> Vec<Frame> {
        let mut ex = MelExtractor::new();
        let mut out = Vec::new();
        ex.push(samples, &mut out);
        out
    }

    fn make_template() -> TemplateFile {
        let mut audio = noise(SAMPLE_RATE as usize / 2, 0.01, 1);
        audio.extend(chime(0.5));
        audio.extend(noise(SAMPLE_RATE as usize / 2, 0.01, 2));
        let trimmed = trim_recording(&frames_of(&audio)).unwrap();
        TemplateFile::from_frames("chest", "test", &trimmed)
    }

    fn run(m: &mut Matcher, samples: &[f32]) -> Vec<Detection> {
        frames_of(samples).into_iter().filter_map(|f| m.push_frame(f)).collect()
    }

    #[test]
    fn ring_buffer_wraps_and_overwrites_oldest() {
        let mut r = RingBuffer::<i32>::new(4);
        r.push_slice(&[1, 2, 3]);
        let mut o = [0; 3];
        assert!(r.peek(&mut o));
        assert_eq!(o, [1, 2, 3]);
        r.discard(2);
        r.push_slice(&[4, 5, 6, 7]);
        assert_eq!(r.len(), 4);
        let mut o = [0; 4];
        assert!(r.peek(&mut o));
        assert_eq!(o, [4, 5, 6, 7]);
        assert!(!r.peek(&mut [0; 5]));
    }

    #[test]
    fn frame_count_matches_stft_geometry() {
        let n = WINDOW + HOP * 9;
        assert_eq!(frames_of(&vec![0.1; n]).len(), 10);
        // chunked input gives identical frames
        let s = noise(20_000, 0.3, 7);
        let one = frames_of(&s);
        let mut ex = MelExtractor::new();
        let mut chunked = Vec::new();
        for c in s.chunks(777) {
            ex.push(c, &mut chunked);
        }
        assert_eq!(one.len(), chunked.len());
        assert_eq!(one[3], chunked[3]);
    }

    #[test]
    fn sine_energy_lands_in_expected_band() {
        let s: Vec<f32> = (0..8192).map(|i| (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / 48000.0).sin()).collect();
        let f = &frames_of(&s)[3];
        let peak = f.iter().enumerate().max_by(|a, b| a.1.partial_cmp(b.1).unwrap()).unwrap().0;
        // 1 kHz should be in the lower third of a 80 Hz - 16 kHz mel scale
        assert!((8..22).contains(&peak), "peak band {peak}");
    }

    #[test]
    fn downmix_averages_channels() {
        let mut o = Vec::new();
        downmix(&[1.0, 3.0, 2.0, 4.0], 2, &mut o);
        assert_eq!(o, vec![2.0, 3.0]);
    }

    #[test]
    fn trim_finds_the_sound() {
        let t = make_template();
        // 0.5 s chime ~ 47 frames, plus padding; must be far shorter than the 1.5 s recording
        assert!(t.frames.len() > 20 && t.frames.len() < 80, "len {}", t.frames.len());
        t.validate().unwrap();
    }

    /// Harsh synthetic game background: a noise bed with loud broadband bursts every 0.4 s.
    fn game_background(secs: f32, seed: u64) -> Vec<f32> {
        let n = (SAMPLE_RATE as f32 * secs) as usize;
        let mut r = Lcg(seed);
        (0..n)
            .map(|i| {
                let t = i as f32 / SAMPLE_RATE as f32;
                let burst = if (t % 0.4) < 0.08 { 0.12 } else { 0.03 };
                r.next() * burst
            })
            .collect()
    }

    #[test]
    fn trim_finds_a_late_sound_in_busy_game_audio() {
        // Regression: a real 4 s in-game recording with the chest near the end. The old trimmer
        // started at the first background burst and capped at 150 frames, cutting the chest off.
        let mut rec = game_background(3.3, 61);
        let bg_tail = game_background(0.7, 62);
        rec.extend(chime(0.5).iter().zip(bg_tail.iter().chain(std::iter::repeat(&0.0))).map(|(c, b)| c + b));
        rec.extend(game_background(0.2, 63));
        let t = trim_recording(&frames_of(&rec)).unwrap();
        assert!(t.len() >= 20 && t.len() <= 80, "template should be about the chime (~47 frames), got {}", t.len());
        // it must detect a chime in fresh background
        let tf = TemplateFile::from_frames("chest", "", &t);
        let mut m = Matcher::new(std::slice::from_ref(&tf), crate::settings::threshold_for(0.5));
        let mut audio = game_background(1.5, 71);
        let tail = game_background(0.5, 72);
        audio.extend(chime(0.5).iter().zip(tail.iter()).map(|(c, b)| c + b));
        audio.extend(game_background(1.5, 73));
        let d = run(&mut m, &audio);
        assert_eq!(d.iter().filter(|d| d.score > 0.8).count(), 1, "detections: {d:?}");
        // These bursts are far harsher than real game audio (live Fortnite background measured
        // <= 0.46, see settings::threshold_for), so check them at a stricter setting.
        let mut m2 = Matcher::new(&[tf], crate::settings::threshold_for(0.25));
        assert!(run(&mut m2, &game_background(6.0, 81)).is_empty());
    }

    #[test]
    fn trim_rejects_silence() {
        let f = frames_of(&noise(48_000, 0.001, 3));
        assert!(trim_recording(&f).is_err());
    }

    #[test]
    fn detects_embedded_sound_once() {
        let t = make_template();
        let mut m = Matcher::new(&[t], crate::settings::threshold_for(0.5));
        let mut audio = noise(SAMPLE_RATE as usize, 0.01, 11);
        audio.extend(chime(0.5));
        audio.extend(noise(SAMPLE_RATE as usize, 0.01, 12));
        let d = run(&mut m, &audio);
        assert_eq!(d.len(), 1, "detections: {d:?}");
        assert!(d[0].score > 0.7);
    }

    #[test]
    fn detection_is_volume_invariant() {
        let t = make_template();
        let mut m = Matcher::new(&[t], crate::settings::threshold_for(0.5));
        let mut audio = noise(SAMPLE_RATE as usize, 0.001, 21);
        audio.extend(chime(0.05));
        audio.extend(noise(SAMPLE_RATE as usize, 0.001, 22));
        assert_eq!(run(&mut m, &audio).len(), 1);
    }

    #[test]
    fn ignores_noise_silence_and_other_sounds() {
        let t = make_template();
        let mut m = Matcher::new(&[t], crate::settings::threshold_for(0.5));
        assert!(run(&mut m, &noise(SAMPLE_RATE as usize * 3, 0.2, 31)).is_empty());
        assert!(run(&mut m, &vec![0.0; SAMPLE_RATE as usize]).is_empty());
        let mut audio = noise(SAMPLE_RATE as usize / 2, 0.01, 41);
        audio.extend(other_sound());
        audio.extend(noise(SAMPLE_RATE as usize / 2, 0.01, 42));
        assert!(run(&mut m, &audio).is_empty());
    }

    #[test]
    fn two_sounds_apart_give_two_detections() {
        let t = make_template();
        let mut m = Matcher::new(&[t], crate::settings::threshold_for(0.5));
        let mut audio = noise(SAMPLE_RATE as usize / 2, 0.01, 51);
        audio.extend(chime(0.5));
        audio.extend(noise(SAMPLE_RATE as usize, 0.01, 52));
        audio.extend(chime(0.5));
        audio.extend(noise(SAMPLE_RATE as usize / 2, 0.01, 53));
        assert_eq!(run(&mut m, &audio).len(), 2);
    }

    #[test]
    fn template_file_roundtrip_and_validation() {
        let t = make_template();
        let json = serde_json::to_string(&t).unwrap();
        assert_eq!(TemplateFile::parse(&json).unwrap().frames.len(), t.frames.len());
        let mut bad = t.clone();
        bad.n_mels = 3;
        assert!(bad.validate().is_err());
        let mut short = t;
        short.frames.truncate(2);
        assert!(short.validate().is_err());
        let mut nan = make_template();
        nan.frames[3][5] = f32::NAN;
        assert!(nan.validate().is_err(), "a NaN would poison every correlation score");
    }
}
