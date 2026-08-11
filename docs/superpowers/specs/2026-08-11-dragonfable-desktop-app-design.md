# DragonFable Desktop App — Design

Date: 2026-08-11
Status: Approved (brainstorming complete)

## Goal

A bare-minimum **DragonFable-only** desktop player for Ruffle, as a fresh Rust crate in
`/workspace/desktop/`. Linux + Windows (no macOS for now). It hardcodes the game URL,
wraps Ruffle's navigator in the `dragonfable-cache` caching layer (with on-disk
AES-obfuscated cache files), and shows a first-boot disclaimer + save-data migration
setup screen.

## Context

- `/workspace/ruffle` — Ruffle fork (branch `master`, 20 commits ahead of upstream, perf
  patches). Path dependency source for all ruffle crates.
- `/workspace/df-cache-layer` — `dragonfable-cache` crate: a `NavigatorBackend` wrapper
  caching DragonFable traffic (SWF GETs, POST endpoints with ID-based cache keys,
  DragonFable form-data deobfuscation via `decrypt.rs`).
- `/workspace/android` — existing Android app (`itmr.dragonfable`) that wires
  `ExternalNavigatorBackend` → `DragonFableCachingNavigator` → `PlayerBuilder::with_navigator`.
  Reference pattern for the desktop app. Default URL:
  `https://play.dragonfable.com/game/DFLoader.swf`.
- `/workspace/evolved-dragonfable-launcher` — separate Electron+Flash launcher. Source of
  the disclaimer-screen pattern and of save-data migration sources.
- `/workspace/desktop` — currently empty; the new crate lives here.

## Approach

**Fresh minimal crate** (chosen over patching `ruffle_desktop` or copying it). `desktop/`
is its own standalone workspace (own `Cargo.lock`), path-depending on the ruffle fork and
df-cache-layer — the same shape as `android/`. No changes to the ruffle tree.

## Crate layout & dependencies

Package `ruffle-dragonfable` (bin), edition 2024, window title "DragonFable".

All third-party deps added via `cargo add` (latest compatible versions — no
hand-pinned versions). Known dependency families (ruffle crates via path):

- `ruffle_core` (path `../ruffle/core`), features: `audio, mp3, aac, nellymoser, lzma,
  default_compatibility_rules, egui`
- `ruffle_render_wgpu` (path `../ruffle/render/wgpu`)
- `ruffle_video_software` (path `../ruffle/video/software`)
- `ruffle_frontend_utils` (path `../ruffle/frontend-utils`), features: `cpal, fs, navigator`
- `dragonfable-cache` (path `../df-cache-layer`)
- UI/platform: `winit`, `wgpu`, `egui`, `egui-wgpu`, `egui-winit`, `cpal`, `fontdb`,
  `url`, `dirs`, `tracing`, `tracing-subscriber`, `tracing-appender`, `anyhow`,
  `async-task`, `webbrowser`, `arboard`, `sys-locale`
- `[patch.crates-io] swf = { path = "../ruffle/swf" }` — same patch df-cache-layer
  carries, so no dependency pulls the crates.io `swf` copy (type-mismatch safety).
- Windows: `#![windows_subsystem = "windows"]` (no console window).
- Linux build needs the usual system packages (alsa etc.), same as building ruffle desktop.

## Modules

```
desktop/src/
  main.rs      — tracing init (stderr + rotating file log), window/event loop, run App
  config.rs    — constants + dirs (see below) + state.toml read/write
  app.rs       — winit event loop: wgpu surface/device, egui state + overlay,
                 input → player, render movie texture → egui → present, TaskPoll events
  player.rs    — Player construction + WinitExecutor FutureSpawner (async-task +
                 event-loop proxy, mirroring ruffle_desktop)
  ui.rs        — MinimalUiBackend (~100 lines) + overlay state machine
                 (Disclaimer / Setup / Playing / Error) — designated growth point
  navigator.rs — MinimalNavigatorInterface (links → system browser, sockets allowed)
  migration.rs — save-data source detection + copying (pure file logic, unit-testable)
```

`MinimalUiBackend` implements `UiBackend`: cursor visibility/icon, clipboard (arboard),
fullscreen, no-op virtual keyboard, fonts via `fontdb`, file dialogs → `None`.

## Config & directories (via `dirs` crate — XDG on Linux, correct dirs on Windows)

- Cache: `dirs::cache_dir()/dragonfable` (Linux `~/.cache/dragonfable`, Windows
  `%LOCALAPPDATA%\dragonfable`) — **max 512 MB** on desktop.
- Saves (SharedObjects): `dirs::data_local_dir()/dragonfable/SharedObjects`
- App state (disclaimer + migration markers, future settings): `state.toml` in
  `dirs::config_local_dir()/dragonfable/`
- Logs: rotating file in `dirs::data_local_dir()/dragonfable/log/` + stderr.
  Default filter `warn,ruffle=info,dragonfable_cache=info`, overridable via `RUST_LOG`.

## df-cache-layer changes (always-on, both platforms)

New module `src/cipher.rs` (existing `decrypt.rs` untouched — still used for POST form
data):

- **AES-128-GCM** (`aes-gcm` with `stream` feature), key = literal passphrase
  `"ZorbakOwnsYou"` zero-padded to 16 bytes. Obfuscation, not security — chosen over
  plain CTR for integrity detection (truncated/corrupt files are detected, not silently
  garbled).
- **12-byte random nonce** per file (`getrandom`).
- **On-disk format:** `DFCACHE\x00\x01` magic (8 bytes) + nonce (12) + ciphertext +
  tag (16). Magic lets stale files (e.g. legacy raw-format caches on Android devices) be
  detected → deleted → treated as cache miss (re-downloaded); version byte future-proofs
  format changes.
- Plaintext length = on-disk size − 8 − 12 − 16, known from metadata.

`cache.rs` rework:

- `CacheWriter::write` encrypts incrementally (GCM stream API).
- `Cache::open` validates magic; returns a decrypting reader (`CacheFile: Read + Send`)
  + **plaintext** length instead of raw `File` + on-disk length. No-magic files →
  deleted + `None`.
- `Cache::write` wipe-comparison (`decoded_ninja` on old cached bytes) reads the old
  file through decryption.
- `Cache::len` (test helper) reports plaintext length.
- Eviction budget stays on-disk size; `modified()` (mtime, HEAD refresh) unchanged.

`lib.rs` changes:

- `CachedBody::File(Arc<Mutex<File>>, u64)` → `Arc<Mutex<CacheFile>>`.
- `expected_length()` reports plaintext length; `next_chunk` reads via the decrypting
  reader (holds back final 16 tag bytes, verifies at end of stream).

Android: same code path, always-on encryption; existing raw caches self-heal via the
magic-header check on first use.

## Boot flow

```
main → window/event loop
  ├─ state.toml missing disclaimer_accepted → Disclaimer screen → Continue → marker
  ├─ no migration marker → scan sources
  │    ├─ none found → proceed
  │    └─ found → Setup screen: one button per detected source + "Don't copy" → marker
  └─ Playing: build Player (lazily) → fetch_root_movie(DFLoader.swf) → render loop
```

Player construction (all lazy, after setup screens):

- `fontdb` fonts; `ExternalNavigatorBackend` wrapped in `DragonFableCachingNavigator`
  (`base_domain = "play.dragonfable.com"`, `max_cache_bytes = 512 MB`);
  `WgpuRenderBackend`; cpal audio; `DiskStorageBackend` (save dir above);
  `MinimalUiBackend`; `SoftwareVideoBackend`.
- Game URL hardcoded: `https://play.dragonfable.com/game/DFLoader.swf`. No CLI override.
- Per frame: winit events → egui-winit + player input → `player.render()` into texture →
  egui fullscreen letterboxed image → present. `PlayerEvent::TaskPoll` handled for the
  executor.

## First-boot screens

**Disclaimer** (mirrors evolved launcher's warning.html): dark-red themed, text *"This
is a 3rd party launcher that is not supported nor endorsed by Artix Entertainment. By
clicking 'Continue', you agree to use this launcher at your own risk."* → Continue.

**Migration scan** — sources (each = candidate roots + optional `127.0.0.1` handling):

1. **Adobe Flash Player**: `~/.macromedia/Flash_Player` | `%APPDATA%\Macromedia\Flash
   Player`, data under `#SharedObjects/<id>/<domain>/…`
2. **Official Artix Game Launcher**: `~/.config/Artix Game Launcher` |
   `%APPDATA%\Artix Game Launcher`, data under `Pepper Data/Shockwave Flash/
   WritableRoot/#SharedObjects/<id>/<domain>/…`
3. **Evolved DragonFable Launcher** (separate launcher, still a supported source):
   `~/.config/evolved-dragonfable-launcher` | `%APPDATA%\evolved-dragonfable-launcher`,
   same Pepper WritableRoot nesting. Scans the 3 real domains **and** `127.0.0.1`
   (evolved rewrote hosts to its local proxy).

Domains scanned: `play.dragonfable.com`, `dragonlord.battleon.com`,
`dragonfable.battleon.com`. A source "has data" if any domain dir contains files.

**Setup screen** (only when ≥1 source found): list detected sources; one button per
source + **"Don't copy"**. Copy = recursively copy each domain dir into
`<SAVE_DIR>/<domain>/…`, mapping `127.0.0.1` → `play.dragonfable.com`, never
overwriting existing files. `.sol` files copied as-is (Ruffle's `DiskStorageBackend`
layout matches Flash's minus the `#SharedObjects/<id>/` prefix, and Ruffle parses Flash
`.sol` natively). Choice recorded; never shown again.

## Error handling

- Root-movie fetch failure → error overlay (message) + Retry / Quit.
- Panics → `catch_unwind` around the event loop (as ruffle_desktop does), logged,
  error overlay.
- df-cache errors degrade gracefully: misses re-download; refresh failures warn only.

## Testing

- df-cache-layer: GCM roundtrip incl. cross-chunk streaming; magic-header validation
  (stale raw files → miss + deleted); wipe-comparison on encrypted data;
  plaintext-length reporting; nonce uniqueness sanity.
- desktop: `migration.rs` unit tests (source detection with temp-dir fakes, `127.0.0.1`
  remapping, no-overwrite, prefix stripping); `config.rs` dir resolution (XDG env
  override).
- Verification: `cargo test` + `cargo build --release` for both crates.

## Non-goals (for now)

- macOS; CLI URL override; full settings UI; menu bar; migrating caches from other
  launchers (SharedObjects only — caches re-download); real security (obfuscation only);
  game-specific QoL features (that's what evolved-dragonfable-launcher is for).

## Future extension points

- Settings dialog / menu bar → `ui.rs` overlay state machine + `config.rs` state.toml.
- Additional migration sources → one table entry in `migration.rs`.
- More game URLs / profiles → `config.rs` constants.
- Desktop packaging (icons, bundles) → out of scope, noted for later.
