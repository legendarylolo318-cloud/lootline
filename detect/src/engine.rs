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

#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    WaitingForGame,
    Attached { pid: u32 },
    Error(String),
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

struct Recording {
    name: String,
    target_frames: usize,
    frames: Vec<Frame>,
}

fn run(cfg: Config, cmds: Receiver<Command>, emit: impl Fn(Event)) {
    let keys = KeyTracker::start(parse_vk(&cfg.interact_key).unwrap_or(0x45));
    let mut matcher = Matcher::new(&cfg.templates, threshold_for(cfg.sensitivity));
    let mut extractor = MelExtractor::new();
    let mut templates = cfg.templates;
    let mut sensitivity = cfg.sensitivity;

    let mut capture: Option<(u32, Capture, Receiver<CapMsg>)> = None;
    let mut last_scan = Instant::now() - SCAN_INTERVAL;
    let mut last_accept: Option<Instant> = None;
    let mut recording: Option<Recording> = None;
    let mut frames: Vec<Frame> = Vec::with_capacity(16);
    emit(Event::Status(Status::WaitingForGame));

    loop {
        // ---- commands
        while let Ok(c) = cmds.try_recv() {
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
                Command::Record { name, secs } => {
                    if capture.is_some() {
                        let target = (secs / FRAME_SECS).ceil() as usize;
                        recording = Some(Recording { name, target_frames: target, frames: Vec::new() });
                    } else {
                        emit(Event::RecordingDone(Err("Fortnite is not running (nothing to record)".into())));
                    }
                }
            }
        }

        // ---- attach / detach
        if capture.is_none() {
            if last_scan.elapsed() >= SCAN_INTERVAL {
                last_scan = Instant::now();
                if let Some(pid) = find_fortnite_pid() {
                    let (ctx, crx) = mpsc::sync_channel(256);
                    capture = Some((pid, Capture::start(pid, ctx), crx));
                    extractor = MelExtractor::new();
                    matcher = Matcher::new(&templates, threshold_for(sensitivity));
                    emit(Event::Status(Status::Attached { pid }));
                }
            }
            std::thread::sleep(Duration::from_millis(100));
            continue;
        }

        let mut ended: Option<String> = None;
        if let Some((pid, _, crx)) = &capture {
            match crx.recv_timeout(Duration::from_millis(100)) {
                Ok(CapMsg::Audio(mono)) => {
                    frames.clear();
                    extractor.push(&mono, &mut frames);
                    for f in frames.drain(..) {
                        if let Some(rec) = &mut recording {
                            rec.frames.push(f);
                        }
                        if let Some(d) = matcher.push_frame(f) {
                            let window = KEY_WINDOW + Duration::from_secs_f32(d.duration_secs + 0.15);
                            if !keys.held_within(window) {
                                emit(Event::Ignored { reason: "interact key not held", score: d.score });
                            } else if last_accept.is_some_and(|t| t.elapsed() < DEBOUNCE) {
                                emit(Event::Ignored { reason: "debounced", score: d.score });
                            } else {
                                last_accept = Some(Instant::now());
                                emit(Event::ChestDetected { score: d.score, template: d.template });
                            }
                        }
                    }
                    if recording.as_ref().is_some_and(|r| r.frames.len() >= r.target_frames) {
                        if let Some(rec) = recording.take() {
                            let res = trim_recording(&rec.frames)
                                .map(|t| TemplateFile::from_frames(&rec.name, "", &t))
                                .map_err(|e| e.to_string());
                            emit(Event::RecordingDone(res));
                        }
                    }
                }
                Ok(CapMsg::Ended(why)) => ended = Some(why),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    // no audio flowing; make sure the game is still alive
                    if last_scan.elapsed() >= SCAN_INTERVAL {
                        last_scan = Instant::now();
                        if find_fortnite_pid() != Some(*pid) {
                            ended = Some("Fortnite exited".into());
                        }
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => ended = Some("capture thread gone".into()),
            }
        }
        if let Some(why) = ended {
            capture = None; // Drop stops and joins the capture thread
            if let Some(_r) = recording.take() {
                emit(Event::RecordingDone(Err("Fortnite closed during recording".into())));
            }
            if find_fortnite_pid().is_some() && why != "Fortnite exited" && why != "stopped" {
                emit(Event::Status(Status::Error(why)));
            } else {
                emit(Event::Status(Status::WaitingForGame));
            }
            last_scan = Instant::now();
        }
    }
}
