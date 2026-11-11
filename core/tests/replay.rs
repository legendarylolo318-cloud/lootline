//! Offline tuning harness: replay a raw capture (mono 48 kHz f32 LE, as written by
//! `LOOTLINE_AUDIO_DUMP`) through the detector with a template and print the per-second best
//! score. Not run in CI: needs local files.
//!
//! LOOTLINE_REPLAY_AUDIO=audio.f32 LOOTLINE_REPLAY_TEMPLATE=chest.json \
//!   cargo test --release -p lootline-core --test replay -- --ignored --nocapture

use lootline_core::dsp::{Frame, Matcher, MelExtractor, TemplateFile, FRAME_SECS};

#[test]
#[ignore]
fn replay_capture() {
    let (Some(a), Some(t)) = (std::env::var_os("LOOTLINE_REPLAY_AUDIO"), std::env::var_os("LOOTLINE_REPLAY_TEMPLATE"))
    else {
        eprintln!("set LOOTLINE_REPLAY_AUDIO and LOOTLINE_REPLAY_TEMPLATE");
        return;
    };
    let bytes = std::fs::read(a).unwrap();
    let samples: Vec<f32> = bytes.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c)).collect();
    let tpl = TemplateFile::parse(&std::fs::read_to_string(t).unwrap()).unwrap();
    let mut ex = MelExtractor::new();
    let mut frames: Vec<Frame> = Vec::new();
    ex.push(&samples, &mut frames);
    let mut m = Matcher::new(&[tpl], 2.0); // threshold unreachable: we only want the scores
    let per_sec = (1.0 / FRAME_SECS).round() as usize;
    let mut line = String::new();
    for (i, chunk) in frames.chunks(per_sec).enumerate() {
        let mut best = 0.0f32;
        for f in chunk {
            m.push_frame(*f);
            best = best.max(m.last_score());
        }
        line.push_str(&format!("{i:>4}s {best:.2}  "));
        if (i + 1) % 10 == 0 {
            println!("{line}");
            line.clear();
        }
    }
    println!("{line}");
}
