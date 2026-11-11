//! Find Fortnite by enumerating process *names*. Never opens the process.

use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};

pub const FORTNITE_EXE: &str = "FortniteClient-Win64-Shipping.exe";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcEntry {
    pub pid: u32,
    pub parent: u32,
    pub name: String,
}

/// Snapshot of all running processes (pid, parent pid, exe name only).
pub fn list_processes() -> Vec<ProcEntry> {
    let mut out = Vec::new();
    // SAFETY: Toolhelp snapshot is a read-only system query; the handle is our own and is closed.
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
            return out;
        };
        let mut e = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        if Process32FirstW(snap, &mut e).is_ok() {
            loop {
                let len = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(e.szExeFile.len());
                out.push(ProcEntry {
                    pid: e.th32ProcessID,
                    parent: e.th32ParentProcessID,
                    name: String::from_utf16_lossy(&e.szExeFile[..len]),
                });
                if Process32NextW(snap, &mut e).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snap);
    }
    out
}

/// PID of the Fortnite game process, if running.
pub fn find_fortnite_pid() -> Option<u32> {
    pick_fortnite(&list_processes())
}

pub fn pick_fortnite(procs: &[ProcEntry]) -> Option<u32> {
    procs.iter().find(|p| p.name.eq_ignore_ascii_case(FORTNITE_EXE)).map(|p| p.pid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_own_process() {
        let me = std::process::id();
        assert!(list_processes().iter().any(|p| p.pid == me));
    }

    #[test]
    fn picks_fortnite_case_insensitively() {
        let v = vec![
            ProcEntry { pid: 1, parent: 0, name: "explorer.exe".into() },
            ProcEntry { pid: 42, parent: 1, name: "fortniteclient-win64-shipping.exe".into() },
        ];
        assert_eq!(pick_fortnite(&v), Some(42));
        assert_eq!(pick_fortnite(&v[..1]), None);
    }
}
