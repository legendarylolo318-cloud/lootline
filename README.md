# Lootline

Fortnite chest tracker for you and your friends: counts chests opened, shows each item's per-chest
odds, your chance of having found it given how many chests you opened, and how lucky or unlucky the
party has been. **Windows 10 (2004+) / 11 only**, like Fortnite itself.

> [!WARNING]
> **The loot table values are placeholders.** The bundled `loot_tables/placeholder.json` rates were
> supplied by hand from a public odds page and have not been verified against Epic's data. Every odds,
> luck and dry-streak number in the app is only as good as that table, and the app shows a
> "PLACEHOLDER loot table" banner until a real one is loaded. See [Loot table](#loot-table).

![Dashboard](docs/dashboard.png)
![Odds](docs/odds.png)
![Party](docs/party.png)

## Download
Grab `lootline-<version>-windows-x86_64.exe` from the [latest release](https://github.com/legendarylolo318-cloud/lootline/releases/latest)
(a single portable exe; `SHA256SUMS.txt` is attached alongside it). Settings, themes and the database live in
`%APPDATA%\lootline\Lootline`.

## Features
- **Audio chest detection**: Fortnite-only process-loopback audio -> STFT (1024/512) -> 40-band
  log-mel -> sliding normalised cross-correlation against template files. Counts only if *your* interact
  key (default E) was held in the 1.5 s before the sound; 2 s debounce; sensitivity slider; runs on its own thread.
- **Templates** live in `templates/`, fetched from the repo, plus an in-app **Record template** tool.
- **Manual fallback**: global hotkeys, F8 add chest, F9 undo.
- **Rarity overlay** (optional, **off by default**; enable in Settings): borderless always-on-top, no-activate window
  with one-click Common..Mythic buttons and an item dropdown that appears after each chest; auto-hides after 3 s
  (chest stays counted, rarity unknown). Position, size, timeout configurable.
- **Party** (iroh, peer to peer, no server, no port forwarding): host gets a copyable code, others paste it.
  Append-only event log merged by set union; newcomers and reconnects exchange what they are missing; host can kick;
  host leaving hands over to the next member. **The party total is the sum of everyone's chests.**
- **Odds**: per item and rarity, for this match / session / lifetime and me / party: expected = n·p,
  P(≥1) = 1−(1−p)^n, luck = Binomial CDF, dry streak = (1−p)^n, all computed in log space so 1-in-a-million
  items stay exact. Hover any number for the formula with real values.
- **History** (sessions, matches, per-player, rarity bars), SQLite storage with versioned migrations.
- **Themes**: 6 built in (Dark, Light, OLED Black, Storm, High Contrast, Ember), TOML files in the themes folder,
  in-app editor with live preview, import/export, themeable rarity colours with reset. Every colour, radius and
  font in the UI comes from the theme. Tray icon with chest count.

## Loot table
`loot_tables/<season>.json` is fetched from this repo, cached, with a bundled fallback.
**The bundled values are placeholders** (`"placeholder": true`): per-chest rates (1 in N) for the weekly treasures,
Pirate Token and Gizmo, taken by hand from a public odds page and not verified. To use real values, publish a
`loot_tables/<season>.json` with `"placeholder": false`, set the season in Settings and press *Fetch latest loot table*.
Audio cannot tell what a chest contained, so which item you got is tagged by hand in the overlay.

## Anti-cheat safety (Easy Anti-Cheat)
Everything happens outside the game. Lootline **never**: opens a handle to the Fortnite process (no `OpenProcess`,
`ReadProcessMemory`, `WriteProcessMemory`), injects DLLs, hooks DirectX/rendering, sniffs packets, or sends/simulates input.
It **only** uses:
- `CreateToolhelp32Snapshot` to read process *names* and find Fortnite's PID (a system-wide listing; the process is not opened).
- The Windows 10 2004+ process-loopback audio API (`ActivateAudioInterfaceAsync`, via the `wasapi` crate) for that PID.
  Only Fortnite's own audio is captured: never Discord, music or your microphone.
- `GetAsyncKeyState` polling for your own interact key (only while Fortnite is running), and `RegisterHotKey`
  (`global-hotkey`) for the hotkeys.
- Style flags and size/position on Lootline's *own* windows (no-activate, topmost overlay; fitting the main window
  to the screen's work area).

The optional single-frame Windows Graphics Capture is not implemented. All game-adjacent APIs live in `detect/src`;
`app/src/platform.rs` only touches Lootline's own windows. The source and the Windows API features enabled for each crate were audited for this before v0.1.0.

## Build
```
cargo test                        # core, detect, net, app unit tests
cargo run -p lootline --release
cargo test -p lootline-net --test party -- --ignored                          # real two-peer iroh test (needs internet)
cargo test --release -p lootline-core --test dsp_bench -- --ignored --nocapture  # detection CPU cost
```
Developer aids: `LOOTLINE_DEMO=1` runs with a fake in-memory party; `LOOTLINE_PAGE=0..4` picks the start page;
`LOOTLINE_SNAPSHOT=<dir>` renders every page in every theme at several window sizes to images (add
`SLINT_SCALE_FACTOR=1.5` to check 150% DPI).

CI (`.github/workflows/ci.yml`) runs `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` and
`cargo test` on every pull request; pushing a `v*` tag builds the release exe and attaches it to a GitHub Release.

## Renderer choice and performance
Slint's **software renderer** (winit backend) is used: no GPU context or driver allocations (idle RAM stays low),
the overlay cannot contend with the game for the GPU, and nothing redraws while idle (0 fps measured).
Glow effects are plain rounded rings and only animate while a Legendary/Mythic is highlighted.
Measured on a release build: about 8–11 MB private working set at idle; detection costs ~0.04% of one core
per template; the key poller and audio capture sleep entirely while Fortnite isn't running. The network
thread and endpoint are created only when you host or join.

## Layout
`core` events, odds, loot tables, storage, themes, DSP (pure, tested) · `detect` Windows audio + keys · `net` iroh party · `app` Slint UI.

MIT licensed, see [LICENSE](LICENSE).
