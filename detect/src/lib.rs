//! Windows-only chest detection: Fortnite process-loopback audio + interact-key polling.
//!
//! ANTI-CHEAT SAFETY: this crate never opens a handle to the Fortnite process. It finds the
//! process id by enumerating process *names* (a Toolhelp snapshot, which is a system-wide
//! query and does not open the target), asks Windows for a loopback stream of that process's
//! audio session (ActivateAudioInterfaceAsync, handled by the audio service), and polls
//! `GetAsyncKeyState` for the user's own interact key. No OpenProcess, no memory access, no
//! injection, no hooks, no input synthesis.

#![cfg(windows)]

pub mod capture;
pub mod engine;
pub mod keys;
pub mod process;

pub use engine::{spawn, Command, Config, DetectorHandle, Event, Status};
