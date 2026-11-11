//! Detection thread: finds Fortnite, attaches process-loopback capture, runs the DSP pipeline,
//! gates matches on the interact key, debounces, and supports recording new templates.
//! Runs entirely off the UI thread; results are delivered through a callback.

use crate::capture::{CapMsg, Capture};
use crate::keys::{parse_vk, KeyTracker};
use crate::process::find_fortnite_pid;
use lootline_core::dsp::{trim_recording, Frame, Matcher, MelExtractor, TemplateFile, FRAME_SECS};
use lootline_core::settings::threshold_for;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

pub const DEBOUNCE: Duration = Duration::from_secs(2);
pub const KEY_WINDOW: Duration = Duration::from_millis(1500);
const SCAN_INTERVAL: Duration = Duration::from_secs(2);
/// Extra slack after the requested recording length before a recording is given up on (the
/// game sends no audio packets at all while it is silent).
const RECORD_GRACE: Duration = Duration::from_secs(3);
const MAX_RETRY_DELAY: Duration = Duration::from_secs(60);

#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    WaitingForGame,
    Attached {
        pid: u32,
    },
    Error(String),
    /// Detection switched off in settings: no capture, no DSP.
    Paused,
}

#[derive(Debug)]
pub enum Event {
    Status(Status),
    ChestDetected {
        score: f32,
        template: String,
    },
    /// A match was found but rejected (interact key not held, or inside the debounce window).
    Ignored {
        reason: &'static str,
        score: f32,
    },
    RecordingDone(Result<TemplateFile, String>),
}

pub enum Command {
    SetSensitivity(f32),
    SetInteractKey(String),
    SetTemplates(Vec<TemplateFile>),
    /// Turn detection on or off. Off releases the audio stream entirely.
    SetEnabled(bool),
    /// Capture `secs` seconds of Fortnite audio and trim it to a template.
    Record {
        name: String,
        secs: f32,
    },
    Shutdown,
}

pub struct Config {
    pub sensitivity: f32,
    pub interact_key: String,
    pub templates: Vec<TemplateFile>,
    pub enabled: bool,
}

pub struct DetectorHandle {
    tx: Sender<Command>,
    join: Option<JoinHandle<()>>,
}

impl DetectorHandle {
    pub fn send(&self, c: Command) {
        let _ = self.tx.send(c);
    }
}

impl Drop for DetectorHandle {
    fn drop(&mut self) {
        let _ = self.tx.send(Command::Shutdown);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

pub fn spawn(cfg: Config, on_event: impl Fn(Event) + Send + 'static) -> DetectorHandle {
    let (tx, rx) = mpsc::channel();
    let join = std::thread::Builder::new().name("lootline-detect".into()).spawn(move || run(cfg, rx, on_event)).ok();
    DetectorHandle { tx, join }
}

/// Accept/reject logic for a matcher hit, separated from the thread so it can be unit-tested.
#[derive(Default)]
pub struct Gate {
    last_accept: Option<Instant>,
}

impl Gate {
    /// `key_held_within(w)` answers "was the interact key down at some point in the last `w`?".
    /// The detection fires at the *end* of a sound of `sound_secs`, so the window is measured
    /// from the start of the sound: KEY_WINDOW before it, plus the sound itself, plus slack.
    pub fn judge(
        &mut self,
        now: Instant,
        sound_secs: f32,
        key_held_within: impl Fn(Duration) -> bool,
    ) -> Result<(), &'static str> {
        let sound = Duration::from_secs_f32(sound_secs.clamp(0.0, 10.0) + 0.15);
        if !key_held_within(KEY_WINDOW + sound) {
            return Err("interact key not held");
        }
        if self.last_accept.is_some_and(|t| now.saturating_duration_since(t) < DEBOUNCE) {
            return Err("debounced");
        }
        self.last_accept = Some(now);
        Ok(())
    }
}

/// Exponential retry delay for re-attaching after a capture *error* while the game keeps
/// running (2 s, 4 s, 8 s ... capped), so a persistent failure doesn't spin up a capture
/// thread every couple of seconds. Reset on success or when the game exits.
#[derive(Default)]
pub struct Backoff {
    failures: u32,
}

impl Backoff {
    pub fn delay(&self) -> Duration {
        if self.failures == 0 {
            return SCAN_INTERVAL;
        }
        (SCAN_INTERVAL * 2u32.saturating_pow(self.failures.min(8))).min(MAX_RETRY_DELAY)
    }
    pub fn fail(&mut self) {
        self.failures = self.failures.saturating_add(1);
    }
    pub fn reset(&mut self) {
        self.failures = 0;
    }
}

struct Recording {
    name: String,
    target_frames: usize,
    frames: Vec<Frame>,
    deadline: Instant,
}

fn finish_recording(rec: Recording) -> Result<TemplateFile, String> {
    trim_recording(&rec.frames).map(|t| TemplateFile::from_frames(&rec.name, "", &t)).map_err(|e| e.to_string())
}

fn run(cfg: Config, cmds: Receiver<Command>, emit: impl Fn(Event)) {
    let keys = KeyTracker::start(parse_vk(&cfg.interact_key).unwrap_or(0x45));
    let mut matcher = Matcher::new(&cfg.templates, threshold_for(cfg.sensitivity));
    let mut extractor = MelExtractor::new();
    let mut templates = cfg.templates;
    let mut sensitivity = cfg.sensitivity;
    let mut enabled = cfg.enabled;

    let mut capture: Option<(u32, Capture, Receiver<CapMsg>)> = None;
    let mut last_scan: Option<Instant> = None;
    let mut backoff = Backoff::default();
    let mut gate = Gate::default();
    let mut recording: Option<Recording> = None;
    let mut frames: Vec<Frame> = Vec::with_capacity(16);
    let mut pending: Option<Command> = None;
    emit(Event::Status(if enabled { Status::WaitingForGame } else { Status::Paused }));

    loop {
        // ---- commands
        while let Some(c) = pending.take().or_else(|| cmds.try_recv().ok()) {
            match c {
                Command::Shutdown => return,
                Command::SetSensitivity(s) => {
                    sensitivity = s;
                    matcher.threshold = threshold_for(s);
                }
                Command::SetInteractKey(k) => {
                    if let Some(vk) = parse_vk(&k) {
                        keys.set_key(vk);
                    }
                }
                Command::SetTemplates(t) => {
                    templates = t;
                    matcher = Matcher::new(&templates, threshold_for(sensitivity));
                }
                Command::SetEnabled(on) => {
                    if on != enabled {
                        enabled = on;
                        if !on {
                            capture = None; // Drop stops and joins the capture thread
                            if recording.take().is_some() {
                                emit(Event::RecordingDone(Err("detection was switched off".into())));
                            }
                            emit(Event::Status(Status::Paused));
                        } else {
                            last_scan = None;
                            backoff.reset();
                            emit(Event::Status(Status::WaitingForGame));
                        }
                    }
                }
                Command::Record { name, secs } => {
                    if capture.is_some() {
                        let secs = secs.clamp(0.5, 15.0);
                        let target = (secs / FRAME_SECS).ceil() as usize;
                        recording = Some(Recording {
                            name,
                            target_frames: target,
                            frames: Vec::with_capacity(target),
                            deadline: Instant::now() + Duration::from_secs_f32(secs) + RECORD_GRACE,
                        });
                    } else if !enabled {
                        emit(Event::RecordingDone(Err("detection is switched off".into())));
                    } else {
                        emit(Event::RecordingDone(Err("Fortnite is not running (nothing to record)".into())));
                    }
                }
            }
        }

        if !enabled {
            // block on the command channel instead of polling: no wakeups while paused
            match cmds.recv() {
                Ok(c) => pending = Some(c),
                Err(_) => return,
            }
            continue;
        }

        // ---- attach / detach
        if capture.is_none() {
            if last_scan.is_none_or(|t| t.elapsed() >= backoff.delay()) {
                last_scan = Some(Instant::now());
                if let Some(pid) = find_fortnite_pid() {
                    let (ctx, crx) = mpsc::sync_channel(64);
                    capture = Some((pid, Capture::start(pid, ctx), crx));
                    extractor = MelExtractor::new();
                    matcher = Matcher::new(&templates, threshold_for(sensitivity));
                    emit(Event::Status(Status::Attached { pid }));
                } else {
                    backoff.reset();
                }
            }
            std::thread::sleep(Duration::from_millis(100));
            continue;
        }

        let mut ended: Option<String> = None;
        if let Some((pid, cap, crx)) = &capture {
            match crx.recv_timeout(Duration::from_millis(250)) {
                Ok(CapMsg::Audio(mono)) => {
                    backoff.reset();
                    // No templates and nothing to record: nothing can match, skip the DSP.
                    if !matcher.is_empty() || recording.is_some() {
                        frames.clear();
                        extractor.push(&mono, &mut frames);
                        for f in frames.drain(..) {
                            if let Some(rec) = &mut recording {
                                rec.frames.push(f);
                            }
                            if let Some(d) = matcher.push_frame(f) {
                                match gate.judge(Instant::now(), d.duration_secs, |w| keys.held_within(w)) {
                                    Ok(()) => emit(Event::ChestDetected { score: d.score, template: d.template }),
                                    Err(reason) => emit(Event::Ignored { reason, score: d.score }),
                                }
                            }
                        }
                    }
                    cap.recycle(mono);
                }
                Ok(CapMsg::Ended(why)) => ended = Some(why),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    // no audio flowing; make sure the game is still alive
                    if last_scan.is_none_or(|t| t.elapsed() >= SCAN_INTERVAL) {
                        last_scan = Some(Instant::now());
                        if find_fortnite_pid() != Some(*pid) {
                            ended = Some("Fortnite exited".into());
                        }
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => ended = Some("capture thread gone".into()),
            }
        }

        // ---- recording completion (by length, or by deadline when the game went quiet)
        let rec_done =
            recording.as_ref().is_some_and(|r| r.frames.len() >= r.target_frames || Instant::now() >= r.deadline);
        if rec_done {
            if let Some(rec) = recording.take() {
                emit(Event::RecordingDone(finish_recording(rec)));
            }
        }

        if let Some(why) = ended {
            capture = None; // Drop stops and joins the capture thread
            if recording.take().is_some() {
                emit(Event::RecordingDone(Err("Fortnite closed during recording".into())));
            }
            let game_alive = find_fortnite_pid().is_some();
            if game_alive && why != "Fortnite exited" && why != "stopped" {
                backoff.fail();
                emit(Event::Status(Status::Error(why)));
            } else {
                backoff.reset();
                emit(Event::Status(Status::WaitingForGame));
            }
            last_scan = Some(Instant::now());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_requires_key_in_window() {
        let mut g = Gate::default();
        let t0 = Instant::now();
        // detection fires at the end of a 0.5 s sound; key pressed 1.0 s before the sound began
        let pressed_ago = Duration::from_millis(1500);
        assert_eq!(g.judge(t0, 0.5, |w| pressed_ago <= w), Ok(()));
        // key pressed 2.5 s before the sound began: outside the 1.5 s window
        let mut g = Gate::default();
        let pressed_ago = Duration::from_millis(3000);
        assert_eq!(g.judge(t0, 0.5, |w| pressed_ago <= w), Err("interact key not held"));
        // same 3 s, but the sound itself lasted 1.5 s: the press was 1.5 s before it began
        let mut g = Gate::default();
        assert_eq!(g.judge(t0, 1.5, |w| pressed_ago <= w), Ok(()));
    }

    #[test]
    fn gate_debounces_for_two_seconds() {
        let mut g = Gate::default();
        let t0 = Instant::now();
        let held = |_: Duration| true;
        assert_eq!(g.judge(t0, 0.5, held), Ok(()));
        assert_eq!(g.judge(t0 + Duration::from_millis(1999), 0.5, held), Err("debounced"));
        assert_eq!(g.judge(t0 + Duration::from_millis(2000), 0.5, held), Ok(()));
        // a rejected hit (key not held) does not restart the debounce window
        let mut g = Gate::default();
        assert_eq!(g.judge(t0, 0.5, |_| false), Err("interact key not held"));
        assert_eq!(g.judge(t0 + Duration::from_millis(10), 0.5, held), Ok(()));
    }

    #[test]
    fn backoff_grows_and_caps() {
        let mut b = Backoff::default();
        assert_eq!(b.delay(), SCAN_INTERVAL);
        b.fail();
        assert_eq!(b.delay(), Duration::from_secs(4));
        b.fail();
        assert_eq!(b.delay(), Duration::from_secs(8));
        for _ in 0..50 {
            b.fail();
        }
        assert_eq!(b.delay(), MAX_RETRY_DELAY);
        b.reset();
        assert_eq!(b.delay(), SCAN_INTERVAL);
    }

    #[test]
    fn recording_times_out_with_what_it_has() {
        // a recording that got no audio at all must still finish (with an error), not hang
        let r = Recording { name: "x".into(), target_frames: 400, frames: vec![], deadline: Instant::now() };
        assert!(finish_recording(r).is_err());
    }
}
