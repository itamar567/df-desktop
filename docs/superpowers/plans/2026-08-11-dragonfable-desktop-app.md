# DragonFable Desktop App Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A bare-minimum DragonFable-only desktop player (Linux/Windows) built on the Ruffle fork, using the `dragonfable-cache` navigator wrapper whose cache files are now AES-CTR + HMAC-SHA256-obfuscated, with a first-boot disclaimer and save-data migration setup screen.

**Architecture:** Two independent crates. (A) `df-cache-layer/` gains an encryption module (`cipher.rs`) and the cache read/write paths are reworked to stream through it (magic-header format, always-on for both Android and desktop). (B) `desktop/` becomes a new standalone bin crate (`ruffle-dragonfable`) with modules `main/app/player/ui/navigator/config/migration`, modeling the proven ruffle_desktop patterns (winit 0.30 + wgpu 27 + egui 0.33, `WinitExecutor` FutureSpawner, `ExternalNavigatorBackend` wrapped in `DragonFableCachingNavigator`). The ruffle fork tree is not modified.

**Tech Stack:** Rust (edition 2024), winit, wgpu, egui/egui-wgpu/egui-winit, cpal (via ruffle_frontend_utils), fontdb, dirs, tracing, AES-128-CTR + HMAC-SHA256 (aes, ctr, hmac, sha2 crates), tokio (dev-only, tests), url, anyhow, async-task, arboard, webbrowser, sys-locale.

## Global Constraints

- Game URL is hardcoded: `https://play.dragonfable.com/game/DFLoader.swf`. **No CLI args at all** (no URL override, no flags).
- Cache max: **512 MB** on desktop (`CACHE_MAX_BYTES = 512 * 1024 * 1024`). Android keeps 256 MB.
- Cache encryption: AES-128-CTR keystream with an HMAC-SHA256 tag (encrypt-then-MAC), key = literal bytes `ZorbakOwnsYou` zero-padded to 16 bytes, used for both CTR and HMAC. **Always on, no flag** — Android and desktop alike. On-disk format: magic `DFCACHE\x00` (8) + nonce (12) + ciphertext + HMAC-SHA256 tag (32). Constant overhead 52 bytes; plaintext length = on-disk size − 52. Streams in 64KB chunks with bounded memory; the HMAC covers the ciphertext and is verified at EOF, so corruption/truncation is detected (loud error, never silent garbage). Legacy raw-format cache files self-heal: detected via bad magic → deleted → cache miss. (Chosen over GCM after the plan review: the aes-gcm crate's streaming API tags every segment, which breaks constant-overhead streaming; CTR+HMAC gives constant overhead + true streaming.)
- Directories via the `dirs` crate (XDG on Linux, `%APPDATA%`/`%LOCALAPPDATA%` on Windows): cache `dirs::cache_dir()/dragonfable`, saves `dirs::data_local_dir()/dragonfable/SharedObjects`, app state `state.toml` in `dirs::config_local_dir()/dragonfable/`, logs in `dirs::data_local_dir()/dragonfable/log/`.
- `base_domain` for the caching navigator: `play.dragonfable.com`.
- First-boot flow: (1) disclaimer screen (text: "This is a 3rd party launcher that is not supported nor endorsed by Artix Entertainment. By clicking 'Continue', you agree to use this launcher at your own risk.") → Continue → (2) if any migration source has save data, a setup screen listing each detected source with one button each plus "Don't copy" → (3) game. Both choices persist in `state.toml`; neither screen is ever shown again.
- Migration sources and domain mapping: Adobe Flash Player (`~/.macromedia/Flash_Player`, `%APPDATA%\Macromedia\Flash Player`), Official Artix Game Launcher, Evolved DragonFable Launcher (both launchers under their config dir + `Pepper Data/Shockwave Flash/WritableRoot`). Domains: `play.dragonfable.com`, `dragonlord.battleon.com`, `dragonfable.battleon.com`; the Evolved source also scans `127.0.0.1` which maps to `play.dragonfable.com`. Copy = strip `#SharedObjects/<id>/` prefix, never overwrite existing files.
- Platforms: Linux + Windows only. `#![windows_subsystem = "windows"]`.
- No macOS support. No settings UI. No file dialogs (UiBackend returns `None`/canceled).
- Version constraints (forced by the ruffle fork's workspace pins, not chosen freely): `wgpu = 27` (ruffle_render_wgpu), `winit = 0.30` and `egui = 0.33` (egui-wgpu/egui-winit 0.33 pair with wgpu 27 + winit 0.30). All other third-party deps: `cargo add` latest.
- Do NOT enable the `ruffle_core/egui` feature (keeps our egui version unconstrained except for the wgpu/winit lockstep above).
- Coding rules: unit tests for all pure logic (one test per branch), DRY, follow ruffle_desktop patterns, reference it by path for glue code, small focused files, commit per task.

## File Structure

**`/workspace/df-cache-layer`** (existing crate, own git repo, branch `master`):
- Create: `src/cipher.rs` — AES-CTR + HMAC-SHA256 obfuscation primitives: `EncryptingWriter`, `DecryptingReader`, magic/format constants, key/nonce derivation.
- Modify: `src/cache.rs` — `CacheWriter` encrypts + finalizes tag; `Cache::open` validates magic, returns decrypting reader + plaintext length; wipe-comparison reads decrypted bytes; tests updated/added.
- Modify: `src/lib.rs` — `CachedBody::File` uses the new `CacheFile` type; `expected_length` = plaintext length.
- Modify: `Cargo.toml` — add `aes-gcm` (features `stream`, `alloc`) and `getrandom`.

**`/workspace/desktop`** (new crate, own git repo, branch `main` — repo already initialized, first commit exists):
- Create: `Cargo.toml`, `.gitignore` (has `/target`), `src/main.rs`, `src/config.rs`, `src/app.rs`, `src/player.rs`, `src/ui.rs`, `src/navigator.rs`, `src/migration.rs`.
- Reference (read-only, patterns to mirror): `/workspace/ruffle/desktop/src/app.rs`, `/workspace/ruffle/desktop/src/gui/controller.rs`, `/workspace/ruffle/desktop/src/gui/movie.rs`, `/workspace/ruffle/desktop/src/player.rs`, `/workspace/ruffle/desktop/src/backends/ui.rs`, `/workspace/ruffle/desktop/src/backends/navigator.rs`, `/workspace/ruffle/desktop/src/util.rs` (key-mapping helpers).

---

# Part A — df-cache-layer encryption

Work in `/workspace/df-cache-layer`.

### Task 1: `src/cipher.rs` — AES-128-GCM primitives

**Files:**
- Create: `src/cipher.rs`
- Test: `src/cipher.rs` (inline `#[cfg(test)] mod tests`)
- Modify: `Cargo.toml` (add deps)

**Interfaces:**
- Consumes: nothing (standalone).
- Produces:
  - `pub(crate) const MAGIC: &[u8; 8]` = `b"DFCACHE\x00"`
  - `pub(crate) const NONCE_LEN: usize` = 12
  - `pub(crate) const TAG_LEN: usize` = 32 (HMAC-SHA256)
  - `pub(crate) const HEADER_LEN: usize` = 20 (magic + nonce)
  - `pub(crate) const OVERHEAD: usize` = 52 (header + tag)
  - `pub(crate) fn random_nonce() -> [u8; 12]`
  - `pub(crate) struct EncryptingWriter<W: Write>` with `new(inner: W, nonce: [u8; 12]) -> Self`, `write(&mut self, bytes: &[u8]) -> io::Result<()>`, `finish(self) -> io::Result<W>` (writes the HMAC tag, flushes, returns inner)
  - `pub(crate) struct DecryptingReader<R: Read>` with `new(inner: R, nonce: [u8; 12]) -> Self`, implementing `Read` (streams decryption with bounded memory, verifies the HMAC at EOF, errors on corruption/truncation)

- [ ] **Step 1: Add dependencies**

```bash
cd /workspace/df-cache-layer
cargo add aes ctr hmac sha2
cargo add getrandom
```

(`ctr` re-exports the `cipher` traits `KeyIvInit`/`StreamCipher`; `hmac` 0.12 and `sha2` 0.10 with default features. All resolve to latest.)

- [ ] **Step 2: Write the failing tests**

Append to a new `src/cipher.rs` (start with tests, then the implementation below):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    fn obfuscate(bytes: &[u8]) -> Vec<u8> {
        // Replicates `Cache::begin_write` (Task 2): the caller writes
        // `MAGIC || nonce`; the writer itself is header-less.
        let nonce = random_nonce();
        let mut file = Vec::new();
        file.extend_from_slice(MAGIC);
        file.extend_from_slice(&nonce);
        let mut writer = EncryptingWriter::new(file, nonce);
        writer.write(bytes).unwrap();
        writer.finish().unwrap()
    }

    fn deobfuscate(file: &[u8]) -> Vec<u8> {
        assert_eq!(&file[..MAGIC.len()], MAGIC, "expected magic header");
        let mut nonce = [0u8; NONCE_LEN];
        nonce.copy_from_slice(&file[MAGIC.len()..HEADER_LEN]);
        let mut reader = DecryptingReader::new(&file[HEADER_LEN..], nonce);
        let mut out = Vec::new();
        reader.read_to_end(&mut out).unwrap();
        out
    }

    #[test]
    fn roundtrip_across_sizes_and_chunk_boundaries() {
        // One test per interesting size: empty, partial/full block, and the
        // 64KB cache chunk boundaries.
        for size in [0, 1, 15, 16, 17, 4096, 65535, 65536, 65537, 100_000] {
            let payload: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
            let file = obfuscate(&payload);
            assert_eq!(file.len(), HEADER_LEN + payload.len() + TAG_LEN);
            assert_eq!(deobfuscate(&file), payload, "roundtrip failed for size {size}");
        }
    }

    #[test]
    fn roundtrip_with_odd_write_chunks() {
        let payload: Vec<u8> = (0..70_000).map(|i| (i % 251) as u8).collect();
        let nonce = random_nonce();
        let mut writer = EncryptingWriter::new(Vec::new(), nonce);
        for part in payload.chunks(7) {
            writer.write(part).unwrap();
        }
        let file = writer.finish().unwrap();
        let mut nonce2 = [0u8; NONCE_LEN];
        nonce2.copy_from_slice(&file[MAGIC.len()..HEADER_LEN]);
        let mut reader = DecryptingReader::new(&file[HEADER_LEN..], nonce2);
        let mut out = Vec::new();
        reader.read_to_end(&mut out).unwrap();
        assert_eq!(out, payload);
    }

    #[test]
    fn corrupted_ciphertext_is_detected() {
        let file = obfuscate(b"attack at dawn");
        let mut corrupted = file.clone();
        let middle = corrupted.len() / 2;
        corrupted[middle] ^= 0xFF;
        let mut reader = DecryptingReader::new(&corrupted[HEADER_LEN..], random_nonce());
        assert!(reader.read_to_end(&mut Vec::new()).is_err());
    }

    #[test]
    fn truncated_file_is_detected() {
        let file = obfuscate(b"attack at dawn");
        let mut reader = DecryptingReader::new(&file[HEADER_LEN..file.len() - 8], random_nonce());
        assert!(reader.read_to_end(&mut Vec::new()).is_err());
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p dragonfable-cache --lib cipher`
Expected: FAIL — module/type not found (`EncryptingWriter`, `DecryptingReader` don't exist yet).

- [ ] **Step 4: Implement the cipher module**

Write `src/cipher.rs`:

```rust
//! AES-128-CTR + HMAC-SHA256 obfuscation for cached files ("DFCACHE" format).
//!
//! On-disk layout: `DFCACHE\x00` magic (8 bytes) || 12-byte nonce ||
//! AES-CTR ciphertext || 32-byte HMAC-SHA256 tag.
//!
//! Encrypt-then-MAC: the tag covers the ciphertext and is verified at EOF, so
//! corrupted or truncated entries fail loudly instead of decrypting to garbage.
//! This is obfuscation, not security: the passphrase matches DragonFable's own
//! obfuscation key and is compiled into the binary.

use std::io::{self, Read, Write};

use aes::Aes128;
use ctr::cipher::{KeyIvInit, StreamCipher};
use ctr::Ctr128BE;
use hmac::{Hmac, Mac};
use sha2::Sha256;

pub(crate) const MAGIC: &[u8; 8] = b"DFCACHE\x00";
pub(crate) const NONCE_LEN: usize = 12;
pub(crate) const TAG_LEN: usize = 32;
pub(crate) const HEADER_LEN: usize = MAGIC.len() + NONCE_LEN;
pub(crate) const OVERHEAD: usize = HEADER_LEN + TAG_LEN;

const PASSPHRASE: &[u8; 13] = b"ZorbakOwnsYou";

fn passphrase_key() -> [u8; 16] {
    let mut key = [0u8; 16];
    key[..PASSPHRASE.len()].copy_from_slice(PASSPHRASE);
    key
}

/// CTR's 16-byte initial counter block: the 12-byte nonce plus a big-endian
/// 32-bit block counter starting at 0.
fn counter_block(nonce: &[u8; NONCE_LEN]) -> [u8; 16] {
    let mut block = [0u8; 16];
    block[..NONCE_LEN].copy_from_slice(nonce);
    block
}

pub(crate) fn random_nonce() -> [u8; NONCE_LEN] {
    let mut nonce = [0u8; NONCE_LEN];
    getrandom::fill(&mut nonce).expect("OS RNG must be available");
    nonce
}

/// Streams plaintext through AES-128-CTR into the wrapped writer, feeding the
/// ciphertext into an incremental HMAC-SHA256. `finish` appends the tag.
///
/// Header-less: the caller (`Cache::begin_write`) writes `MAGIC || nonce`
/// before construction.
pub(crate) struct EncryptingWriter<W: Write> {
    inner: W,
    cipher: Ctr128BE<Aes128>,
    mac: Hmac<Sha256>,
}

impl<W: Write> EncryptingWriter<W> {
    pub(crate) fn new(inner: W, nonce: [u8; NONCE_LEN]) -> Self {
        let key = passphrase_key();
        Self {
            inner,
            cipher: Ctr128BE::<Aes128>::new(&key.into(), &counter_block(&nonce).into()),
            mac: Hmac::<Sha256>::new_from_slice(&key).expect("16-byte HMAC key is valid"),
        }
    }

    pub(crate) fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        let mut encrypted = bytes.to_vec();
        self.cipher.apply_keystream(&mut encrypted);
        self.mac.update(&encrypted);
        self.inner.write_all(&encrypted)
    }

    pub(crate) fn finish(mut self) -> io::Result<W> {
        self.inner.write_all(&self.mac.finalize().into_bytes())?;
        self.inner.flush()?;
        Ok(self.inner)
    }
}

/// Streams decryption of a "DFCACHE" payload (everything after the header).
///
/// Holds back the trailing 32 bytes (the tag) while decoding the ciphertext
/// in bounded memory; the final read verifies the HMAC over the full
/// ciphertext, failing loudly on any corruption or truncation.
pub(crate) struct DecryptingReader<R: Read> {
    inner: R,
    cipher: Ctr128BE<Aes128>,
    mac: Hmac<Sha256>,
    pending: Vec<u8>,
    out: Vec<u8>,
    out_pos: usize,
    finished: bool,
}

impl<R: Read> DecryptingReader<R> {
    pub(crate) fn new(inner: R, nonce: [u8; NONCE_LEN]) -> Self {
        let key = passphrase_key();
        Self {
            inner,
            cipher: Ctr128BE::<Aes128>::new(&key.into(), &counter_block(&nonce).into()),
            mac: Hmac::<Sha256>::new_from_slice(&key).expect("16-byte HMAC key is valid"),
            pending: Vec::new(),
            out: Vec::new(),
            out_pos: 0,
            finished: false,
        }
    }

    /// Reads more ciphertext, decoding everything that is guaranteed not to
    /// be the trailing tag.
    fn fill(&mut self) -> io::Result<()> {
        let mut chunk = [0u8; 64 * 1024];
        let read = self.inner.read(&mut chunk)?;
        self.pending.extend_from_slice(&chunk[..read]);
        if read == 0 {
            // EOF: the last 32 bytes are the tag, the rest the final
            // ciphertext (usually empty — mid-stream feeds already consumed
            // everything but the tag).
            let split = self.pending.len().saturating_sub(TAG_LEN);
            let (body, tag) = self.pending.split_at(split);
            let mut plaintext = body.to_vec();
            self.cipher.apply_keystream(&mut plaintext);
            self.mac.update(body);
            let expected = self.mac.clone().finalize().into_bytes();
            if expected.as_slice() != tag {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "cache entry failed integrity check",
                ));
            }
            self.out = plaintext;
            self.out_pos = 0;
            self.finished = true;
            return Ok(());
        }
        if self.pending.len() > TAG_LEN {
            let feed = self.pending.len() - TAG_LEN;
            let body: Vec<u8> = self.pending.drain(..feed).collect();
            let mut plaintext = body.clone();
            self.cipher.apply_keystream(&mut plaintext);
            self.mac.update(&body);
            self.out = plaintext;
            self.out_pos = 0;
        }
        Ok(())
    }
}

impl<R: Read> Read for DecryptingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        while self.out_pos == self.out.len() {
            if self.finished {
                return Ok(0);
            }
            self.fill()?;
        }
        let available = self.out.len() - self.out_pos;
        let n = buf.len().min(available);
        buf[..n].copy_from_slice(&self.out[self.out_pos..self.out_pos + n]);
        self.out_pos += n;
        if self.out_pos == self.out.len() {
            self.out.clear();
            self.out_pos = 0;
        }
        Ok(n)
    }
}
```

Notes for the implementer: `Ctr128BE::<Aes128>::new(&key.into(), &iv.into())` takes 16-byte `GenericArray`s — `.into()` from `[u8; 16]` works. `apply_keystream` (from `ctr::cipher::StreamCipher`) mutates in place and advances the keystream position, so successive calls continue seamlessly. `Hmac::<Sha256>` implements `Clone`, which the EOF verify uses to avoid consuming the running MAC. If exact trait paths differ across the resolved crate versions, check `cargo doc -p ctr -p hmac -p sha2`. Bounded memory: at most one 64KB chunk plus the 32 held-back tag bytes.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p dragonfable-cache --lib cipher`
Expected: all cipher tests PASS.

- [ ] **Step 6: Commit**

```bash
cd /workspace/df-cache-layer
git add Cargo.toml Cargo.lock src/cipher.rs src/lib.rs
git commit -m "Add AES-CTR + HMAC obfuscation for cached files"
```

---

### Task 2: Wire encryption through `cache.rs` and `lib.rs`

**Files:**
- Modify: `src/cache.rs`
- Modify: `src/lib.rs`

**Interfaces:**
- Consumes: `cipher::{random_nonce, DecryptingReader, EncryptingWriter, HEADER_LEN, MAGIC, NONCE_LEN, OVERHEAD}` (Task 1).
- Produces:
  - `pub(crate) type CacheFile = DecryptingReader<std::io::BufReader<std::fs::File>>`
  - `Cache::open(&self, key) -> Option<(CacheFile, u64)>` — validates magic, deletes stale files, returns decrypting reader + **plaintext** length
  - `Cache::begin_write(&self, key) -> io::Result<CacheWriter>` — writes header (magic + fresh nonce)
  - `CacheWriter::write(&mut self, bytes)` unchanged signature, now encrypts
  - `CacheWriter::finish(&mut self) -> io::Result<()>` — finalizes tag (called from `commit`)
  - `Cache::read(&self, key) -> Option<Vec<u8>>` (test helper) now returns decrypted bytes
  - `Cache::len(&self, key) -> Option<u64>` now returns plaintext length
  - `lib.rs`: `CachedBody::File(Arc<Mutex<cache::CacheFile>>, u64)`

- [ ] **Step 1: Update the failing test first**

In `src/cache.rs` tests, update `evicts_oldest_when_over_cap`: with encryption each 6-byte payload occupies 42 bytes on disk (36 overhead + 6), so under a 12-byte budget every file gets evicted:

```rust
#[tokio::test]
async fn evicts_oldest_when_over_cap() {
    let (_dir, cache) = temp_cache(12);
    cache.write("aaa", b"123456", true).await;
    cache.write("bbb", b"123456", true).await;
    cache.write("ccc", b"123456", true).await;
    // Every entry is 58 bytes on disk (52 bytes of encryption overhead +
    // 6 bytes of payload), so none can fit under the 12-byte budget.
    assert!(cache.len("aaa").await.is_none());
    assert!(cache.len("bbb").await.is_none());
    assert!(cache.len("ccc").await.is_none());
    assert_eq!(cache.total.load(Ordering::Relaxed), 0);
}
```

And add three new tests:

```rust
#[tokio::test]
async fn stale_legacy_file_is_removed_and_reported_as_miss() {
    let (dir, cache) = temp_cache(1024);
    // A raw, unencrypted file from before the encryption change.
    std::fs::write(cache.path("legacy"), b"<ninja2>raw data</ninja2>").unwrap();
    assert!(cache.open("legacy").await.is_none());
    assert!(!dir.path().join("legacy").exists(), "stale file must be deleted");
}

#[tokio::test]
async fn tampered_cache_file_fails_to_read() {
    let (_dir, cache) = temp_cache(1024);
    cache.write("abc", b"attack at dawn", true).await;
    let mut file = std::fs::read(cache.path("abc")).unwrap();
    let middle = file.len() / 2;
    file[middle] ^= 0xFF;
    std::fs::write(cache.path("abc"), &file).unwrap();
    assert_eq!(cache.read("abc").await, None, "tampering must be detected");
}

#[tokio::test]
async fn fresh_nonce_per_file() {
    let (_dir, cache) = temp_cache(1024);
    cache.write("one", b"first", true).await;
    cache.write("two", b"second", true).await;
    let one = std::fs::read(cache.path("one")).unwrap();
    let two = std::fs::read(cache.path("two")).unwrap();
    assert_ne!(&one[..HEADER_LEN], &two[..HEADER_LEN], "nonces must differ");
}
```

`cache.path` is currently private to the module — the tests live inside `mod tests` in the same file so it stays visible (it already is, since the existing tests use `cache.path`? they don't — they use `cache.len`/`cache.read`. Keep `path` `fn` private but callable from the inline tests; it already is (`fn path` without `pub`). No change needed).

- [ ] **Step 2: Run the new tests to verify they fail**

Run: `cargo test -p dragonfable-cache --lib cache`
Expected: FAIL — `stale_legacy_file_is_removed_and_reported_as_miss` fails (`open` currently accepts any file), `tampered_cache_file_fails_to_read` fails (no decryption), `fresh_nonce_per_file` fails (no header).

- [ ] **Step 3: Rework `src/cache.rs`**

Full new `src/cache.rs` (replaces the existing file — keep `temp_path`, `wipe`, `walk_entries`, `modified`, `decoded_ninja` as-is):

```rust
use std::fs::File;
use std::io::{self, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::SystemTime;

use crate::cipher::{self, DecryptingReader, EncryptingWriter, HEADER_LEN, MAGIC, OVERHEAD};

pub(crate) const CHUNK_SIZE: usize = 64 * 1024;

/// An encrypted cache entry, streamed through GCM decryption.
pub(crate) type CacheFile = DecryptingReader<BufReader<File>>;

pub(crate) struct CacheWriter {
    writer: Option<EncryptingWriter<File>>,
    temp: PathBuf,
    path: PathBuf,
}

impl CacheWriter {
    pub(crate) fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.writer.as_mut().unwrap().write(bytes)
    }

    pub(crate) fn finish(&mut self) -> io::Result<()> {
        let writer = self.writer.take().unwrap();
        writer.finish()?;
        Ok(())
    }
}

impl Drop for CacheWriter {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.temp);
    }
}

/// On-disk cache store.
pub(crate) struct Cache {
    root: PathBuf,
    max_bytes: u64,
    total: AtomicI64,
}

impl Cache {
    pub(crate) fn new(root: PathBuf, max_bytes: u64) -> Self {
        if let Err(error) = std::fs::create_dir_all(&root) {
            log::warn!("could not create cache dir {}: {error}", root.display());
        }
        let total = walk_entries(&root)
            .iter()
            .map(|(_, len, _)| *len as i64)
            .sum();
        Self {
            root,
            max_bytes,
            total: AtomicI64::new(total),
        }
    }

    fn path(&self, key: &str) -> PathBuf {
        debug_assert!(key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_')));
        self.root.join(key)
    }

    pub(crate) async fn open(&self, key: &str) -> Option<(CacheFile, u64)> {
        let path = self.path(key);
        let mut file = match File::open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return None,
            Err(error) => {
                log::warn!("cache open failed for {key}: {error}");
                return None;
            }
        };
        let mut header = [0u8; HEADER_LEN];
        if file.read_exact(&mut header).is_err() || &header[..MAGIC.len()] != MAGIC {
            // Stale file from an older format (or foreign data); drop it so it
            // is treated as a miss and re-downloaded.
            log::warn!("cache entry {key} has an unknown format; removing it");
            let _ = std::fs::remove_file(&path);
            return None;
        }
        let file_len = match file.metadata() {
            Ok(metadata) => metadata.len(),
            Err(error) => {
                log::warn!("cache metadata read failed for {key}: {error}");
                return None;
            }
        };
        if file_len < OVERHEAD as u64 {
            log::warn!("cache entry {key} is truncated; removing it");
            let _ = std::fs::remove_file(&path);
            return None;
        }
        let plaintext_len = file_len - OVERHEAD as u64;
        let mut nonce = [0u8; cipher::NONCE_LEN];
        nonce.copy_from_slice(&header[MAGIC.len()..]);
        let reader = DecryptingReader::new(BufReader::new(file), nonce);
        Some((reader, plaintext_len))
    }

    pub(crate) async fn modified(&self, key: &str) -> Option<SystemTime> {
        std::fs::metadata(self.path(key)).ok()?.modified().ok()
    }

    pub(crate) fn begin_write(&self, key: &str) -> io::Result<CacheWriter> {
        let temp = self.temp_path(key);
        let mut file = File::create(&temp)?;
        file.write_all(MAGIC)?;
        let nonce = cipher::random_nonce();
        file.write_all(&nonce)?;
        Ok(CacheWriter {
            writer: Some(EncryptingWriter::new(file, nonce)),
            temp,
            path: self.path(key),
        })
    }

    pub(crate) async fn commit(&self, mut writer: CacheWriter) -> io::Result<()> {
        writer.finish()?;
        let new_len = std::fs::metadata(&writer.temp)?.len() as i64;
        let old_len = std::fs::metadata(&writer.path)
            .map(|metadata| metadata.len() as i64)
            .unwrap_or(0);
        std::fs::rename(&writer.temp, &writer.path)?;
        self.total.fetch_add(new_len - old_len, Ordering::Relaxed);
        self.evict_if_needed().await;
        Ok(())
    }

    pub(crate) async fn read(&self, key: &str) -> Option<Vec<u8>> {
        let (mut reader, len) = self.open(key).await?;
        let mut bytes = Vec::with_capacity(len as usize);
        reader.read_to_end(&mut bytes).ok()?;
        Some(bytes)
    }

    pub(crate) async fn len(&self, key: &str) -> Option<u64> {
        self.open(key).await.map(|(_, len)| len)
    }

    pub(crate) async fn write(&self, key: &str, bytes: &[u8], should_wipe_cache_on_change: bool) {
        if should_wipe_cache_on_change
            && self.read(key).await.is_some_and(|old| {
                decoded_ninja(&old)
                    .zip(decoded_ninja(bytes))
                    .map_or(old != bytes, |(old, new)| old != new)
            })
        {
            // A differing entry means the game data changed, so every cached
            // response is stale and must be re-downloaded.
            log::info!("wiping cache because cached response changed for {key}");
            self.wipe().await;
        }
        let mut writer = match self.begin_write(key) {
            Ok(writer) => writer,
            Err(error) => {
                log::warn!("cache write failed for {key}: {error}");
                return;
            }
        };
        if let Err(error) = writer.write(bytes) {
            log::warn!("cache write failed for {key}: {error}");
            return;
        }
        if let Err(error) = self.commit(writer).await {
            log::warn!("cache commit failed for {key}: {error}");
        }
    }

    pub(crate) async fn wipe(&self) {
        match std::fs::remove_dir_all(&self.root) {
            Ok(()) => log::info!("cache cleared"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => log::warn!("cache wipe failed: {error}"),
        }
        if let Err(error) = std::fs::create_dir_all(&self.root) {
            log::warn!("cache dir recreation failed: {error}");
        }
        self.total.store(0, Ordering::Relaxed);
    }

    fn temp_path(&self, key: &str) -> PathBuf {
        self.root.join(format!(
            ".tmp-{}-{}",
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed),
            key
        ))
    }

    async fn evict_if_needed(&self) {
        let total = self.total.load(Ordering::Relaxed);
        if total <= self.max_bytes as i64 {
            return;
        }
        log::info!(
            "cache reached {total} bytes (limit {} bytes); LRU eviction needed",
            self.max_bytes
        );
        let mut entries = walk_entries(&self.root);
        entries.sort_by_key(|(mtime, _, _)| *mtime);
        for (_, len, path) in entries {
            if self.total.load(Ordering::Relaxed) <= self.max_bytes as i64 {
                break;
            }
            match std::fs::remove_file(&path) {
                Ok(()) => {
                    self.total.fetch_sub(len as i64, Ordering::Relaxed);
                    log::info!("cache LRU evicted {} ({len} bytes)", path.display());
                }
                Err(error) => log::warn!("cache eviction failed for {}: {error}", path.display()),
            }
        }
    }
}

static NEXT_TEMP: AtomicI64 = AtomicI64::new(0);

fn decoded_ninja(bytes: &[u8]) -> Option<String> {
    let document = roxmltree::Document::parse(std::str::from_utf8(bytes).ok()?).ok()?;
    let root = document.root_element();
    (root.tag_name().name() == "ninja2")
        .then_some(root.text()?)
        .and_then(crate::decrypt::decrypt)
}

fn walk_entries(root: &Path) -> Vec<(SystemTime, u64, PathBuf)> {
    // Unchanged from before.
    let mut entries = Vec::new();
    let Ok(read_dir) = std::fs::read_dir(root) else {
        return entries;
    };
    for entry in read_dir.flatten() {
        let path = entry.path();
        if entry.file_name().to_string_lossy().starts_with(".tmp-") {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if meta.is_file() {
            let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            entries.push((mtime, meta.len(), path));
        }
    }
    entries
}
```

(Keep the existing `mod tests` and add the updated/extra tests from Step 1. Note `Cache::write`'s wipe comparison now compares **decrypted** old bytes — `decoded_ninja` operates on plaintext as before.)

- [ ] **Step 4: Rework the two call sites in `src/lib.rs`**

In `src/lib.rs`:

1. Change the import: `use cache::{Cache, CacheFile, CacheWriter, CHUNK_SIZE};` (add `CacheFile`).
2. Change `CachedBody`:

```rust
enum CachedBody {
    File(Arc<Mutex<CacheFile>>, u64),
    Memory(Vec<u8>),
}
```

3. In `fetch_cached` the `cache.open(&key).await` result is already destructured as `Some((file, len))` — the `len` is now the plaintext length; the `CachedResponse`/`StreamingResponse` code compiles unchanged because `CacheFile` implements `Read` (`read_to_end`, `read`, `next_chunk` all keep working).

- [ ] **Step 5: Run the full test suite**

Run: `cargo test -p dragonfable-cache`
Expected: all unit tests (cipher + cache + decrypt) and `tests/navigator.rs` integration tests PASS. The integration tests exercise the public `DragonFableCachingNavigator` API and are format-agnostic — they must not change.

- [ ] **Step 6: Commit**

```bash
cd /workspace/df-cache-layer
git add src/cache.rs src/lib.rs
git commit -m "Encrypt cache entries with AES-128-GCM at rest"
```

---

# Part B — desktop app

Work in `/workspace/desktop`.

### Task 3: Scaffold the crate and resolve dependencies

**Files:**
- Create: `Cargo.toml`, `src/main.rs` (temporary hello-world), keep `.gitignore` (`/target`)

**Interfaces:**
- Consumes: nothing.
- Produces: a compiling crate with all dependencies wired, so later tasks only add module code.

- [ ] **Step 1: `cargo init`**

```bash
cd /workspace/desktop
cargo init --name ruffle-dragonfable --vcs none
```

(`--vcs none` because the repo already exists. `cargo init` creates `src/main.rs` + `Cargo.toml`.)

- [ ] **Step 2: Add path dependencies**

```bash
cargo add ruffle_core --path ../ruffle/core --features audio,mp3,aac,nellymoser,lzma,default_compatibility_rules
cargo add ruffle_render_wgpu --path ../ruffle/render/wgpu
cargo add ruffle_video_software --path ../ruffle/video/software
cargo add ruffle_frontend_utils --path ../ruffle/frontend-utils --features cpal,fs,navigator
cargo add dragonfable-cache --path ../df-cache-layer
```

- [ ] **Step 3: Add third-party dependencies**

```bash
# wgpu/winit/egui are pinned to the versions the ruffle fork's workspace uses
# (ruffle_render_wgpu requires wgpu 27; egui-wgpu/egui-winit 0.33 pair with
# wgpu 27 and winit 0.30). Everything else resolves to latest.
cargo add wgpu@27
cargo add winit@0.30
cargo add egui@0.33 egui-wgpu@0.33 egui-winit@0.33
cargo add fontdb url dirs anyhow futures tracing tracing-subscriber tracing-appender async-task webbrowser arboard sys-locale
```

- [ ] **Step 4: Add the swf patch**

Append to `Cargo.toml`:

```toml
# The in-tree swf fork must be used everywhere, so no dependency pulls the
# crates.io copy (which would cause type mismatches).
[patch.crates-io]
swf = { path = "../ruffle/swf" }
```

- [ ] **Step 5: Build**

Run: `cargo build`
Expected: compiles (hello-world main). If dependency resolution fails, the usual fix is aligning egui/winit versions with the ruffle workspace pins (`egui = "0.33.3"`, `winit = "0.30.13"` are what ruffle_desktop uses — `cargo add egui@0.33.3` etc. if the resolver picks an incompatible 0.33.x).

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "Scaffold ruffle-dragonfable desktop crate"
```

---

### Task 4: `src/config.rs` — directories and persisted state

**Files:**
- Create: `src/config.rs`
- Test: `src/config.rs` (inline tests)

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `pub const GAME_URL: &str`, `pub const BASE_DOMAIN: &str`, `pub const CACHE_MAX_BYTES: u64`
  - `pub fn cache_dir() -> PathBuf`, `pub fn save_dir() -> PathBuf`, `pub fn config_dir() -> PathBuf`, `pub fn log_dir() -> PathBuf`
  - `#[derive(Serialize, Deserialize)] pub struct State { pub disclaimer_accepted: bool, pub migration: Option<MigrationChoice> }` with `pub fn load(config_dir: &Path) -> Self` and `pub fn save(&self, config_dir: &Path) -> io::Result<()>`
  - `pub struct MigrationChoice { pub source: Option<String>, pub copied_at_unix: i64 }`

- [ ] **Step 1: Add dependencies**

```bash
cargo add serde --features derive
cargo add toml
```

- [ ] **Step 2: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn set_var(key: &str, value: Option<&str>) {
        match value {
            Some(value) => std::env::set_var(key, value),
            None => std::env::remove_var(key),
        }
    }

    // Env vars are process-global, so all dir assertions live in one test to
    // avoid races with other tests.
    #[test]
    fn dirs_follow_xdg_environment() {
        let original_cache = std::env::var_os("XDG_CACHE_HOME");
        let original_data = std::env::var_os("XDG_DATA_HOME");
        let original_config = std::env::var_os("XDG_CONFIG_HOME");

        set_var("XDG_CACHE_HOME", Some("/tmp/df-test-cache"));
        set_var("XDG_DATA_HOME", Some("/tmp/df-test-data"));
        set_var("XDG_CONFIG_HOME", Some("/tmp/df-test-config"));
        assert_eq!(cache_dir(), PathBuf::from("/tmp/df-test-cache/dragonfable"));
        assert_eq!(
            save_dir(),
            PathBuf::from("/tmp/df-test-data/dragonfable/SharedObjects")
        );
        assert_eq!(
            config_dir(),
            PathBuf::from("/tmp/df-test-config/dragonfable")
        );
        assert_eq!(log_dir(), PathBuf::from("/tmp/df-test-data/dragonfable/log"));

        set_var("XDG_CACHE_HOME", original_cache.as_deref());
        set_var("XDG_DATA_HOME", original_data.as_deref());
        set_var("XDG_CONFIG_HOME", original_config.as_deref());
    }

    #[test]
    fn state_roundtrips_through_toml() {
        let dir = tempfile::tempdir().unwrap();
        let state = State {
            disclaimer_accepted: true,
            migration: Some(MigrationChoice {
                source: Some("flash-player".into()),
                copied_at_unix: 1234,
            }),
        };
        state.save(dir.path()).unwrap();
        let loaded = State::load(dir.path());
        assert_eq!(loaded, state);
        assert!(dir.path().join("state.toml").exists());
    }

    #[test]
    fn missing_state_file_loads_as_default() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(State::load(dir.path()), State::default());
    }
}
```

Need `tempfile` as a dev-dependency: `cargo add --dev tempfile`.

- [ ] **Step 3: Run to verify they fail**

Run: `cargo test config`
Expected: FAIL — `config` module doesn't exist.

- [ ] **Step 4: Implement `src/config.rs`**

```rust
//! App configuration: platform directories and persisted state.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const GAME_URL: &str = "https://play.dragonfable.com/game/DFLoader.swf";
pub const BASE_DOMAIN: &str = "play.dragonfable.com";
pub const CACHE_MAX_BYTES: u64 = 512 * 1024 * 1024;

fn join(app_dir: Option<PathBuf>, name: &str) -> PathBuf {
    app_dir.expect("could not determine platform directory").join(name)
}

/// `$XDG_CACHE_HOME/dragonfable` (Linux) / `%LOCALAPPDATA%\dragonfable` (Windows).
pub fn cache_dir() -> PathBuf {
    join(dirs::cache_dir(), "dragonfable")
}

/// `$XDG_DATA_HOME/dragonfable/SharedObjects` (Linux) / `%APPDATA%\dragonfable\SharedObjects` (Windows).
pub fn save_dir() -> PathBuf {
    join(dirs::data_local_dir(), "dragonfable/SharedObjects")
}

/// `$XDG_CONFIG_HOME/dragonfable` (Linux) / `%APPDATA%\dragonfable` (Windows).
pub fn config_dir() -> PathBuf {
    join(dirs::config_local_dir(), "dragonfable")
}

/// `$XDG_DATA_HOME/dragonfable/log` (Linux) / `%APPDATA%\dragonfable\log` (Windows).
pub fn log_dir() -> PathBuf {
    join(dirs::data_local_dir(), "dragonfable/log")
}

/// First-boot state persisted as `state.toml` in [`config_dir`].
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct State {
    pub disclaimer_accepted: bool,
    pub migration: Option<MigrationChoice>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MigrationChoice {
    /// Which source data was copied from; `None` means the user chose not to copy.
    pub source: Option<String>,
    pub copied_at_unix: i64,
}

impl State {
    pub fn load(config_dir: &Path) -> Self {
        match std::fs::read(config_dir.join("state.toml")) {
            Ok(bytes) => toml::from_str(&String::from_utf8_lossy(&bytes)).unwrap_or_default(),
            Err(_) => State::default(),
        }
    }

    pub fn save(&self, config_dir: &Path) -> io::Result<()> {
        std::fs::create_dir_all(config_dir)?;
        let encoded = toml::to_string(self).map_err(io::Error::other)?;
        std::fs::write(config_dir.join("state.toml"), encoded)
    }
}
```

Note: `dirs` reads the environment on every call, which is what makes the XDG tests deterministic.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test config`
Expected: PASS (3 tests).

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock src/config.rs
git commit -m "Add config module with dirs and persisted first-boot state"
```

---

### Task 5: `src/migration.rs` — save-data detection and copying

**Files:**
- Create: `src/migration.rs`
- Test: `src/migration.rs` (inline tests)

**Interfaces:**
- Consumes: nothing (pure file logic; `&Path` inputs — no env access inside the tested functions).
- Produces:
  - `pub const DOMAINS: &[&str]` = `["play.dragonfable.com", "dragonlord.battleon.com", "dragonfable.battleon.com"]`
  - `pub struct MigrationSource { pub id: &'static str, pub name: &'static str, pub include_proxy_host: bool, pub roots: Vec<PathBuf> }`
  - `pub fn sources() -> Vec<MigrationSource>` — reads env (`HOME`, `XDG_CONFIG_HOME`, `APPDATA`)
  - `pub fn detect(root: &Path, include_proxy_host: bool) -> Vec<(String, PathBuf)>` — returns `(mapped_domain, source_dir)` pairs for the first `#SharedObjects/<id>/<domain>` found per domain (pure, unit-tested)
  - `pub fn copy_source(domain_dirs: &[(String, PathBuf)], save_dir: &Path) -> io::Result<usize>` — returns number of files copied; never overwrites; maps proxy host already applied in `detect`

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(path: &std::path::Path, content: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    fn flash_like_root() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        write(
            &dir.path().join("#SharedObjects/A4B2/play.dragonfable.com/some.sol"),
            "sol-data",
        );
        write(
            &dir.path().join("#SharedObjects/A4B2/dragonlord.battleon.com/other.sol"),
            "sol-data",
        );
        dir
    }

    #[test]
    fn detects_real_domains() {
        let dir = flash_like_root();
        let detected = detect(dir.path(), false);
        assert_eq!(detected.len(), 2);
        assert!(detected.iter().any(|(domain, _)| domain == "play.dragonfable.com"));
        assert!(detected.iter().any(|(domain, _)| domain == "dragonlord.battleon.com"));
    }

    #[test]
    fn detects_proxy_host_and_maps_it_to_play_domain() {
        let dir = tempfile::tempdir().unwrap();
        write(
            &dir.path().join("#SharedObjects/X1/127.0.0.1/some.sol"),
            "sol-data",
        );
        let detected = detect(dir.path(), true);
        assert_eq!(detected.len(), 1);
        assert_eq!(detected[0].0, "play.dragonfable.com");
        let without_proxy = detect(dir.path(), false);
        assert!(without_proxy.is_empty());
    }

    #[test]
    fn ignores_empty_domain_dirs() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("#SharedObjects/A/play.dragonfable.com")).unwrap();
        assert!(detect(dir.path(), false).is_empty());
    }

    #[test]
    fn no_shared_objects_dir_means_no_data() {
        let dir = tempfile::tempdir().unwrap();
        assert!(detect(dir.path(), false).is_empty());
    }

    #[test]
    fn copy_strips_prefix_and_never_overwrites() {
        let src = flash_like_root();
        let save_dir = tempfile::tempdir().unwrap();
        let detected = detect(src.path(), false);

        // Pre-create one target to prove it is not overwritten.
        let target = save_dir.path().join("play.dragonfable.com/some.sol");
        write(&target, "precious");

        let copied = copy_source(&detected, save_dir.path()).unwrap();
        assert!(copied >= 1);
        assert_eq!(
            fs::read_to_string(target).unwrap(),
            "precious",
            "existing files must never be overwritten"
        );
        assert_eq!(
            fs::read_to_string(save_dir.path().join("dragonlord.battleon.com/other.sol"))
                .unwrap(),
            "sol-data"
        );
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test migration`
Expected: FAIL — `migration` module doesn't exist.

- [ ] **Step 3: Implement `src/migration.rs`**

```rust
//! First-run save-data migration from other DragonFable launchers.
//!
//! Flash stores SharedObjects as `#SharedObjects/<id>/<domain>/<swf-path>/<name>.sol`;
//! Ruffle's DiskStorageBackend stores the same layout under `<save_dir>/<domain>/...`
//! (no `#SharedObjects/<id>` prefix) and parses Flash's .sol format natively, so
//! migration is a straight directory copy per domain.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub const DOMAINS: &[&str] = &[
    "play.dragonfable.com",
    "dragonlord.battleon.com",
    "dragonfable.battleon.com",
];

/// Host that the Evolved DragonFable Launcher stores data under (its local
/// proxy); it maps back to the real game domain.
const PROXY_HOST: &str = "127.0.0.1";

pub struct MigrationSource {
    pub id: &'static str,
    pub name: &'static str,
    /// Whether `127.0.0.1` dirs count as data (and map to `play.dragonfable.com`).
    pub include_proxy_host: bool,
    pub roots: Vec<PathBuf>,
}

/// Candidate source definitions. Reads the environment for platform dirs.
pub fn sources() -> Vec<MigrationSource> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let xdg_config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|home| home.join(".config")));
    let appdata = std::env::var_os("APPDATA").map(PathBuf::from);

    let mut flash_roots = Vec::new();
    if let Some(home) = &home {
        flash_roots.push(home.join(".macromedia").join("Flash_Player"));
    }
    if let Some(appdata) = &appdata {
        flash_roots.push(appdata.join("Macromedia").join("Flash Player"));
    }

    let mut launcher_roots = |app_name: &str| {
        let mut roots = Vec::new();
        if let Some(xdg_config) = &xdg_config {
            roots.push(
                xdg_config
                    .join(app_name)
                    .join("Pepper Data/Shockwave Flash/WritableRoot"),
            );
        }
        if let Some(appdata) = &appdata {
            roots.push(
                appdata
                    .join(app_name)
                    .join("Pepper Data/Shockwave Flash/WritableRoot"),
            );
        }
        roots
    };

    vec![
        MigrationSource {
            id: "flash-player",
            name: "Adobe Flash Player",
            include_proxy_host: false,
            roots: flash_roots,
        },
        MigrationSource {
            id: "artix-game-launcher",
            name: "Artix Game Launcher",
            include_proxy_host: false,
            roots: launcher_roots("Artix Game Launcher"),
        },
        MigrationSource {
            id: "evolved-dragonfable-launcher",
            name: "Evolved DragonFable Launcher",
            include_proxy_host: true,
            roots: launcher_roots("evolved-dragonfable-launcher"),
        },
    ]
}

/// Scans one source root's `#SharedObjects` tree and returns
/// `(mapped_domain, source_dir)` pairs for every domain that has data.
pub fn detect(root: &Path, include_proxy_host: bool) -> Vec<(String, PathBuf)> {
    let shared_objects = match fs::read_dir(root.join("#SharedObjects")) {
        Ok(entries) => entries,
        Err(_) => return Vec::new(),
    };

    let mut wanted: Vec<(&str, String)> =
        DOMAINS.iter().map(|domain| (*domain, (*domain).to_string())).collect();
    if include_proxy_host {
        wanted.push((PROXY_HOST, "play.dragonfable.com".to_string()));
    }

    let mut found = Vec::new();
    let mut wanted: Vec<(&str, String)> = wanted.into_iter().collect();
    'sources: for entry in shared_objects.flatten() {
        if !entry.file_type().is_ok_and(|ty| ty.is_dir()) {
            continue;
        }
        for (source_host, mapped_domain) in &wanted {
            let domain_dir = entry.path().join(source_host);
            if has_data(&domain_dir) {
                found.push((mapped_domain.clone(), domain_dir));
                wanted.retain(|(host, _)| host != source_host);
                if wanted.is_empty() {
                    break 'sources;
                }
            }
        }
    }
    found
}

fn has_data(dir: &Path) -> bool {
    let Ok(entries) = fs::read_dir(dir) else {
        return false;
    };
    entries.flatten().any(|entry| {
        entry.file_type().is_ok_and(|ty| {
            ty.is_file() || (ty.is_dir() && has_data(&entry.path()))
        })
    })
}

/// Recursively copies each domain dir into `<save_dir>/<mapped_domain>/...`,
/// skipping any files that already exist. Returns the number of files copied.
pub fn copy_source(domain_dirs: &[(String, PathBuf)], save_dir: &Path) -> io::Result<usize> {
    let mut copied = 0;
    for (domain, source_dir) in domain_dirs {
        copied += copy_tree(source_dir, &save_dir.join(domain))?;
    }
    Ok(copied)
}

fn copy_tree(source: &Path, target: &Path) -> io::Result<usize> {
    let mut copied = 0;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let from = entry.path();
        let to = target.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copied += copy_tree(&from, &to)?;
        } else if !to.exists() {
            if let Some(parent) = to.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(&from, &to)?;
            copied += 1;
        }
    }
    Ok(copied)
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test migration`
Expected: PASS (5 tests).

- [ ] **Step 5: Commit**

```bash
git add src/migration.rs
git commit -m "Add save-data migration from other DragonFable launchers"
```

---

### Task 6: `src/ui.rs` — screen state machine + minimal `UiBackend`

**Files:**
- Create: `src/ui.rs`
- Test: `src/ui.rs` (screen-transition tests)

**Interfaces:**
- Consumes: `crate::config::State` (Task 4), `crate::migration::MigrationSource` (Task 5).
- Produces:
  - `pub enum Screen { Disclaimer, Setup { sources: Vec<MigratedSource> }, Playing, Error { message: String } }`
  - `pub struct MigratedSource { pub id: String, pub name: String }`
  - `pub fn initial_screen(state: &State, detected: &[MigratedSource]) -> Screen`
  - `pub struct MinimalUiBackend` implementing `ruffle_core::backend::ui::UiBackend`, `pub fn new(window: Arc<winit::window::Window>, font_database: Rc<fontdb::Database>, root_error: Arc<Mutex<Option<String>>>) -> Self`

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::State;

    fn source(id: &str) -> MigratedSource {
        MigratedSource { id: id.into(), name: id.into() }
    }

    #[test]
    fn disclaimer_shows_first_even_with_data_available() {
        let state = State::default();
        let detected = vec![source("flash-player")];
        assert_eq!(initial_screen(&state, &detected), Screen::Disclaimer);
    }

    #[test]
    fn setup_shows_only_when_data_detected() {
        let state = State { disclaimer_accepted: true, ..State::default() };
        let detected = vec![source("flash-player"), source("evolved-dragonfable-launcher")];
        assert_eq!(
            initial_screen(&state, &detected),
            Screen::Setup { sources: detected.clone() }
        );
    }

    #[test]
    fn setup_skipped_when_no_data() {
        let state = State { disclaimer_accepted: true, ..State::default() };
        assert_eq!(initial_screen(&state, &[]), Screen::Playing);
    }

    #[test]
    fn setup_skipped_after_migration_choice() {
        let state = State {
            disclaimer_accepted: true,
            migration: Some(crate::config::MigrationChoice {
                source: None,
                copied_at_unix: 0,
            }),
            ..State::default()
        };
        assert_eq!(initial_screen(&state, &[source("flash-player")]), Screen::Playing);
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test ui`
Expected: FAIL — `ui` module doesn't exist.

- [ ] **Step 3: Implement the screen state machine part of `src/ui.rs`**

```rust
//! Overlay screens (disclaimer / migration setup / game / error) and the
//! minimal `UiBackend` implementation.

use std::path::Path;
use std::sync::{Arc, Mutex};

use fontdb::Database;
use ruffle_core::backend::ui::{
    FileFilter, FontDefinition, FontQuery, MouseCursor, UiBackend,
};
use ruffle_core::swf::{Encoding, LanguageIdentifier};
use ruffle_core::url::Url;
use winit::window::{CursorIcon, Window};

use crate::config::State;

/// Which overlay screen the app is showing. The `ui` module owns the
/// transition logic; the app renders the current screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Screen {
    Disclaimer,
    Setup { sources: Vec<MigratedSource> },
    Playing,
    Error { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigratedSource {
    pub id: String,
    pub name: String,
}

impl From<&crate::migration::MigrationSource> for MigratedSource {
    fn from(source: &crate::migration::MigrationSource) -> Self {
        Self { id: source.id.into(), name: source.name.into() }
    }
}

/// Decides which screen to show after loading persisted state and scanning
/// migration sources. (Actions like "Continue" or "copy from X" are applied
/// by the app, which persists the choice and switches to `Playing`.)
pub fn initial_screen(state: &State, detected: &[MigratedSource]) -> Screen {
    if !state.disclaimer_accepted {
        Screen::Disclaimer
    } else if state.migration.is_none() && !detected.is_empty() {
        Screen::Setup { sources: detected.to_vec() }
    } else {
        Screen::Playing
    }
}
```

- [ ] **Step 4: Add the `MinimalUiBackend` implementation**

Append to `src/ui.rs`:

```rust
/// Minimal `UiBackend`: mouse cursor + clipboard + fullscreen via winit/arboard,
/// device fonts via fontdb, everything else a no-op or `None`.
pub struct MinimalUiBackend {
    window: Arc<Window>,
    font_database: Rc<Database>,
    clipboard: Option<arboard::Clipboard>,
    cursor_visible: bool,
    /// Set by the core when the root movie fails to download; the app reads it
    /// each frame to switch to the error screen.
    pub root_error: Arc<Mutex<Option<String>>>,
}

impl MinimalUiBackend {
    pub fn new(
        window: Arc<Window>,
        font_database: Rc<Database>,
        root_error: Arc<Mutex<Option<String>>>,
    ) -> Self {
        let clipboard = arboard::Clipboard::new().ok();
        window.set_cursor_visible(true);
        Self { window, font_database, clipboard, cursor_visible: true, root_error }
    }
}

impl UiBackend for MinimalUiBackend {
    fn mouse_visible(&self) -> bool {
        self.cursor_visible
    }

    fn set_mouse_visible(&mut self, visible: bool) {
        self.cursor_visible = visible;
        self.window.set_cursor_visible(visible);
    }

    fn set_mouse_cursor(&mut self, cursor: MouseCursor) {
        let icon = match cursor {
            MouseCursor::Arrow => CursorIcon::Default,
            MouseCursor::Hand => CursorIcon::Hand,
            MouseCursor::IBeam => CursorIcon::Text,
            MouseCursor::Grab => CursorIcon::Grab,
        };
        self.window.set_cursor_icon(icon);
    }

    fn clipboard_content(&mut self) -> String {
        self.clipboard.as_mut().and_then(|clipboard| clipboard.get_text().ok()).unwrap_or_default()
    }

    fn set_clipboard_content(&mut self, content: String) {
        if let Some(clipboard) = self.clipboard.as_mut() {
            let _ = clipboard.set_text(content);
        }
    }

    fn set_fullscreen(&mut self, is_full: bool) -> Result<(), ruffle_core::backend::ui::FullscreenError> {
        use winit::window::Fullscreen;
        self.window.set_fullscreen(if is_full { Some(Fullscreen::Borderless(None)) } else { None });
        Ok(())
    }

    fn display_root_movie_download_failed_message(&self, _invalid_swf: bool, fetched_error: String) {
        *self.root_error.lock().unwrap() = Some(fetched_error);
    }

    fn message(&self, message: &str) {
        tracing::warn!("Flash message: {message}");
    }

    fn open_virtual_keyboard(&self) {}

    fn close_virtual_keyboard(&self) {}

    fn language(&self) -> LanguageIdentifier {
        sys_locale::get_locale()
            .and_then(|locale| locale.parse().ok())
            .unwrap_or_else(|| "en-US".parse().expect("en-US is a valid locale"))
    }

    fn display_unsupported_video(&self, url: Url) {
        if url.scheme() == "javascript" {
            tracing::warn!("SWF tried to run a script, but javascript calls are not allowed");
            return;
        }
        if let Err(error) = webbrowser::open(url.as_str()) {
            tracing::error!("Could not open URL {}: {error}", url);
        }
    }

    fn load_device_font(&self, query: &FontQuery, register: &mut dyn FnMut(FontDefinition)) {
        use fontdb::{Family, Query, Style, Weight};

        let query = Query {
            families: &[Family::Name(&query.name)],
            weight: if query.is_bold { Weight::BOLD } else { Weight::NORMAL },
            style: if query.is_italic { Style::Italic } else { Style::Normal },
            ..Default::default()
        };
        if let Some(id) = self.font_database.query(&query)
            && let Some(face) = self.font_database.face(id)
        {
            let is_bold = face.weight > Weight::NORMAL;
            let is_italic = face.style != Style::Normal;
            match &face.source {
                fontdb::Source::File(path) => {
                    if let Ok(data) = std::fs::read(path) {
                        register(FontDefinition::FontFile {
                            name: query.families[0].to_string(),
                            is_bold,
                            is_italic,
                            data: ruffle_core::backend::ui::FontFileData::new_shared(data.into()),
                            index: face.index,
                        });
                    }
                }
                fontdb::Source::Binary(bin) | fontdb::Source::SharedFile(_, bin) => {
                    register(FontDefinition::FontFile {
                        name: query.families[0].to_string(),
                        is_bold,
                        is_italic,
                        data: ruffle_core::backend::ui::FontFileData::new_shared(bin.clone()),
                        index: face.index,
                    });
                }
            }
        }
    }

    fn sort_device_fonts(
        &self,
        _query: &FontQuery,
        _register: &mut dyn FnMut(FontDefinition),
    ) -> Vec<FontQuery> {
        // No fontconfig integration yet; system font fallback covers the game.
        Vec::new()
    }

    fn display_file_open_dialog(
        &mut self,
        _filters: Vec<FileFilter>,
    ) -> Option<ruffle_core::backend::ui::DialogResultFuture> {
        None
    }

    fn display_file_open_dialog_multiple(
        &mut self,
        _filters: Vec<FileFilter>,
    ) -> Option<ruffle_core::backend::ui::MultiDialogResultFuture> {
        None
    }

    fn close_file_dialog(&mut self) {}

    fn display_file_save_dialog(
        &mut self,
        _file_name: String,
        _domain: String,
    ) -> Option<ruffle_core::backend::ui::DialogResultFuture> {
        None
    }
}
```

Note: `FontDefinition`/`FontFileData`/`LanguageIdentifier`/`Url`/`Encoding` re-export paths — check `ruffle_core::backend::ui` and `ruffle_core::swf` for the exact re-exports (the android app imports `ruffle_core::swf::Encoding`, so `swf` is re-exported; `LanguageIdentifier` comes from `ruffee_core::backend::ui` — verify against the compiler if it errors and adjust imports, matching how `NullUiBackend` in `ruffle_core/src/backend/ui.rs` spells the trait methods). The unused `Path`/`Encoding` imports above should be dropped if the compiler flags them.

- [ ] **Step 5: Run tests + build**

Run: `cargo test ui && cargo build`
Expected: 4 ui tests PASS; crate builds (the backend part compiles).

- [ ] **Step 6: Commit**

```bash
git add src/ui.rs
git commit -m "Add screen state machine and minimal UiBackend"
```

---

### Task 7: `src/player.rs` + `src/navigator.rs` — player construction and executor

**Files:**
- Create: `src/player.rs`, `src/navigator.rs`

**Interfaces:**
- Consumes: `crate::config::{CACHE_MAX_BYTES, GAME_URL, BASE_DOMAIN, save_dir}` (Task 4), `crate::ui::MinimalUiBackend` (Task 6).
- Produces:
  - `pub enum RuffleEvent { TaskPoll(async_task::Runnable<()>) }`
  - `pub fn build_player(window: &Arc<Window>, descriptors: &Arc<Descriptors>, event_loop: &EventLoopProxy<RuffleEvent>, font_database: Rc<fontdb::Database>, movie_size: Arc<Mutex<Option<(u32, u32)>>>, root_error: Arc<Mutex<Option<String>>>) -> anyhow::Result<Arc<Mutex<Player>>>` — constructs the navigator (wrapped in `DragonFableCachingNavigator`), renderer (`TextureTarget`), audio, storage, ui backend, and calls `fetch_root_movie` with an `on_metadata` callback that stores the stage size in `movie_size`.
  - `struct WinitExecutor` (private) implementing `ruffle_frontend_utils::backends::navigator::FutureSpawner<E>`

Reference (read these, then adapt): `/workspace/ruffle/desktop/src/player.rs:200-260` (navigator construction), `:541-560` (WinitExecutor), `/workspace/android/src/lib.rs:504-540` (same wiring done in Rust, simpler than desktop's).

- [ ] **Step 1: Write `src/navigator.rs`**

```rust
//! Minimal `NavigatorInterface` for the desktop app.

use std::fs::File;
use std::io;
use std::path::Path;

use ruffle_frontend_utils::backends::navigator::NavigatorInterface;
use url::Url;

#[derive(Clone)]
pub struct MinimalNavigatorInterface;

impl NavigatorInterface for MinimalNavigatorInterface {
    fn navigate_to_website(&self, url: Url) {
        if let Err(error) = webbrowser::open(url.as_str()) {
            tracing::error!("Could not open URL {}: {error}", url);
        }
    }

    async fn open_file(&self, _path: &Path) -> io::Result<File> {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "file access is not supported by this launcher",
        ))
    }

    async fn confirm_socket(&self, _host: &str, _port: u16) -> bool {
        true
    }
}
```

- [ ] **Step 2: Write `src/player.rs`**

```rust
//! Player construction and the winit-backed future executor.

use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Context;
use ruffle_core::player::{Player, PlayerBuilder};
use ruffle_core::{
    Letterbox, LoadBehavior, PlayerRuntime, StageAlign, StageScaleMode,
};
use ruffle_frontend_utils::backends::audio::CpalAudioBackend;
use ruffle_frontend_utils::backends::navigator::{
    ExternalNavigatorBackend, FutureSpawner,
};
use ruffle_frontend_utils::backends::storage::DiskStorageBackend;
use ruffle_frontend_utils::content::{ContentDescriptor, PlayingContent};
use ruffle_render_wgpu::backend::{WgpuRenderBackend, request_adapter_and_device};
use ruffle_render_wgpu::descriptors::Descriptors;
use ruffle_render_wgpu::target::TextureTarget;
use ruffle_video_software::backend::SoftwareVideoBackend;
use url::Url;
use winit::event_loop::EventLoopProxy;
use winit::window::Window;

use crate::config::{self, BASE_DOMAIN, CACHE_MAX_BYTES, GAME_URL};
use crate::navigator::MinimalNavigatorInterface;
use crate::ui::MinimalUiBackend;
use dragonfable_cache::{Config, DragonFableCachingNavigator};

/// Events the winit event loop processes for us.
pub enum RuffleEvent {
    TaskPoll(async_task::Runnable<()>),
}

/// A bare-bones executor that schedules futures on the winit event loop,
/// mirroring ruffle_desktop's `WinitExecutor`.
struct WinitExecutor {
    event_loop: EventLoopProxy<RuffleEvent>,
}

impl<E: std::error::Error + 'static> FutureSpawner<E> for WinitExecutor {
    fn spawn(&self, future: ruffle_core::backend::navigator::OwnedFuture<(), E>) {
        let future = async {
            if let Err(error) = future.await {
                tracing::error!("Async error: {error}");
            }
        };
        let event_loop = self.event_loop.clone();
        let scheduler = move |task| {
            if event_loop.send_event(RuffleEvent::TaskPoll(task)).is_err() {
                tracing::error!("Couldn't schedule task - event loop is closed");
            }
        };
        let (runnable, task) = async_task::Builder::new().spawn_local(|_| future, scheduler);
        task.detach();
        runnable.schedule();
    }
}

#[allow(clippy::too_many_arguments)]
pub fn build_player(
    window: &Arc<Window>,
    descriptors: &Arc<Descriptors>,
    event_loop: &EventLoopProxy<RuffleEvent>,
    font_database: Rc<fontdb::Database>,
    movie_size: Arc<Mutex<Option<(u32, u32)>>>,
    root_error: Arc<Mutex<Option<String>>>,
) -> anyhow::Result<Arc<Mutex<Player>>> {
    let future_spawner = WinitExecutor { event_loop: event_loop.clone() };

    let movie_url = Url::parse(GAME_URL).context("hardcoded game URL must parse")?;
    let base_domain = movie_url.host_str().unwrap_or(BASE_DOMAIN).to_string();

    let navigator = DragonFableCachingNavigator::new(
        ExternalNavigatorBackend::new(
            movie_url.clone(),
            None,
            None,
            future_spawner.clone(),
            None,
            true,
            Default::default(),
            ruffle_core::backend::navigator::SocketMode::Allow,
            Rc::new(PlayingContent::DirectFile(ContentDescriptor::new_remote(movie_url.clone()))),
            MinimalNavigatorInterface,
        ),
        Config {
            cache_dir: config::cache_dir(),
            base_domain,
            max_cache_bytes: CACHE_MAX_BYTES,
        },
        future_spawner,
    );

    let movie_size_px = movie_size.lock().unwrap().unwrap_or((800, 600));
    let renderer = WgpuRenderBackend::new(
        descriptors.clone(),
        TextureTarget::new(&descriptors.device, movie_size_px)?,
    )?;

    let mut builder = PlayerBuilder::new()
        .with_navigator(navigator)
        .with_renderer(renderer)
        .with_storage(Box::new(DiskStorageBackend::new(config::save_dir())))
        .with_ui(Box::new(MinimalUiBackend::new(window.clone(), font_database, root_error)))
        .with_video(SoftwareVideoBackend::new())
        .with_autoplay(true)
        .with_letterbox(Letterbox::On)
        .with_quality(ruffle_render::quality::StageQuality::High)
        .with_max_execution_duration(Duration::MAX);

    if let Ok(audio) = CpalAudioBackend::new(None) {
        builder = builder.with_audio(audio);
    } else {
        tracing::warn!("No audio device available; running without audio");
    }

    let player = builder.build();

    {
        let mut player_lock = player.lock().unwrap();
        let movie_size = movie_size.clone();
        player_lock.fetch_root_movie(GAME_URL.to_string(), Vec::new(), Box::new(move |header| {
            let stage = header.stage_size();
            *movie_size.lock().unwrap() = Some((stage.width().get() / 20, stage.height().get() / 20));
        }));
    }

    Ok(player)
}
```

Notes: `TextureTarget::new` needs the movie dimensions; before metadata arrives the app uses `(800, 600)` as a placeholder and recreates the target when `movie_size` changes (Task 8). `stage.width()` is in Twips (20 twips = 1 px); `Twips::get()` returns i32 — the `(u32, u32)` cast must clamp negatives (SWF headers can't be negative in practice; if the compiler complains, use `max(0, ...) as u32`). `LoadBehavior`/`PlayerRuntime`/`StageAlign`/`StageScaleMode` imports are there for future use — drop them if unused. Verify `TextureTarget::new(&descriptors.device, size)` matches the signature in `/workspace/ruffle/render/wgpu/src/target.rs:236`.

- [ ] **Step 3: Wire the modules into `src/main.rs` temporarily**

Replace the hello-world `main.rs` body with the module declarations so the crate compiles:

```rust
#![windows_subsystem = "windows"]

mod config;
mod migration;
mod navigator;
mod player;
mod ui;

fn main() {}
```

- [ ] **Step 4: Build**

Run: `cargo build`
Expected: compiles. Fix any import/signature drift from the reference files listed above (the compiler will point them out; cross-check against `/workspace/ruffle/desktop/src/player.rs` and `/workspace/ruffle/render/wgpu/src/target.rs`).

- [ ] **Step 5: Commit**

```bash
git add src/main.rs src/player.rs src/navigator.rs
git commit -m "Add player construction with caching navigator and executor"
```

---

### Task 8: `src/app.rs` + real `main.rs` — event loop, rendering, screens

**Files:**
- Create: `src/app.rs`
- Modify: `src/main.rs` (real entry point)

**Interfaces:**
- Consumes: `config::{State, ...}` (T4), `migration::{sources, detect, copy_source, MigrationSource}` (T5), `ui::{Screen, MinimalUiBackend, initial_screen, MigratedSource}` (T6), `player::{build_player, RuffleEvent}` (T7).
- Produces: `pub struct App` implementing `winit::application::ApplicationHandler<RuffleEvent>` with `new(event_loop: &ActiveEventLoop) -> anyhow::Result<Self>`.

Reference (read, then adapt the minimal version): `/workspace/ruffle/desktop/src/app.rs` (input forwarding: lines 95-260; about_to_wait tick loop: lines 362-410; redraw handling: 689-710) and `/workspace/ruffle/desktop/src/gui/controller.rs:40-180` (wgpu instance/surface/device/egui setup) and `:306-450` (render loop: surface texture, egui frame, tessellation, egui_wgpu paint, present).

- [ ] **Step 1: Key setup, rendering, and input code (structure)**

`src/app.rs` (full file; the render and input functions are the core):

```rust
//! The winit application: window, wgpu + egui setup, screen rendering, and
//! input forwarding to the player.

use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{Context, Result};
use egui::ViewportId;
use ruffle_core::events::{ImeEvent, KeyCode, MouseButton, MouseInputSource, MouseWheelDelta, PlayerEvent};
use ruffle_core::FloatDuration;
use ruffle_render_wgpu::backend::{
    create_wgpu_instance, request_adapter_and_device, WgpuRenderBackend,
};
use ruffle_render_wgpu::descriptors::Descriptors;
use ruffle_render_wgpu::target::TextureTarget;
use url::Url;
use wgpu::Backends;
use winit::application::ApplicationHandler;
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, Ime, KeyEvent, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoopProxy};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowId};

use crate::config::{self, State};
use crate::migration;
use crate::player::{RuffleEvent, build_player};
use crate::ui::{MigratedSource, Screen, initial_screen};

pub struct App {
    window: Option<Arc<Window>>,
    event_loop: EventLoopProxy<RuffleEvent>,
    state: State,
    screen: Screen,
    // wgpu + egui, constructed once the window exists
    descriptors: Option<Arc<Descriptors>>,
    egui_winit: Option<egui_winit::State>,
    egui_renderer: Option<egui_wgpu::Renderer>,
    surface: Option<wgpu::Surface<'static>>,
    surface_format: Option<wgpu::TextureFormat>,
    // player state
    player: Option<Arc<Mutex<ruffle_core::player::Player>>>,
    movie_size: Arc<Mutex<Option<(u32, u32)>>>,
    movie_texture: Option<wgpu::Texture>,
    movie_texture_id: Option<egui::TextureId>,
    root_error: Arc<Mutex<Option<String>>>,
    font_database: Rc<fontdb::Database>,
    time: Instant,
    last_pointer: PhysicalPosition<f64>,
}

impl App {
    pub fn new(event_loop: &ActiveEventLoop) -> Result<Self> {
        let event_loop_proxy = event_loop.create_proxy();

        let window = Arc::new(
            event_loop
                .create_window(
                    Window::default_attributes()
                        .with_title("DragonFable")
                        .with_inner_size(PhysicalSize::new(1280, 800)),
                )
                .context("window creation failed")?,
        );

        // wgpu instance + adapter + device (mirrors ruffle_desktop gui/controller.rs:52-118).
        let instance = create_wgpu_instance(Backends::all(), wgpu::BackendOptions::default());
        let surface = unsafe {
            instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::from_window(window.as_ref())?)?
        };
        let (adapter, device, queue) =
            futures::executor::block_on(request_adapter_and_device(
                Backends::all(),
                &instance,
                Some(&surface),
                wgpu::PowerPreference::HighPerformance,
            ))
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let supported_formats = surface.get_capabilities(&adapter).formats;
        let surface_format = supported_formats
            .first()
            .copied()
            .expect("at least one surface format must be supported");
        let size = window.inner_size();
        surface.configure(
            &device,
            &wgpu::SurfaceConfiguration {
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                format: surface_format,
                width: size.width,
                height: size.height,
                present_mode: wgpu::PresentMode::AutoVsync,
                desired_maximum_frame_latency: 2,
                alpha_mode: wgpu::CompositeAlphaMode::Auto,
                view_formats: Vec::new(),
            },
        );

        let descriptors = Arc::new(Descriptors::new(instance, adapter, device, queue));

        // egui setup (mirrors controller.rs:118-145).
        let egui_ctx = egui::Context::default();
        let mut egui_winit = egui_winit::State::new(
            egui_ctx,
            ViewportId::ROOT,
            window.as_ref(),
            None,
            None,
            None,
        );
        egui_winit.set_max_texture_side(descriptors.limits.max_texture_dimension_2d as usize);
        let egui_renderer = egui_wgpu::Renderer::new(
            &descriptors.device,
            surface_format,
            egui_wgpu::RendererOptions {
                msaa_samples: 1,
                depth_stencil_format: None,
                dithering: false,
                predictable_texture_filtering: false,
            },
        );

        // System fonts for the game's device-font text.
        let mut font_database = fontdb::Database::new();
        font_database.load_system_fonts();
        let font_database = Rc::new(font_database);

        // First-boot state + migration scan.
        let state = State::load(&config::config_dir());
        let detected: Vec<MigratedSource> = migration::sources()
            .iter()
            .filter(|source| {
                source.roots.iter().any(|root| {
                    !migration::detect(root, source.include_proxy_host).is_empty()
                })
            })
            .map(Into::into)
            .collect();
        let screen = initial_screen(&state, &detected);

        let movie_size = Arc::new(Mutex::new(None));
        let root_error = Arc::new(Mutex::new(None));

        Ok(Self {
            window: Some(window),
            event_loop: event_loop_proxy,
            state,
            screen,
            descriptors: Some(descriptors),
            egui_winit: Some(egui_winit),
            egui_renderer: Some(egui_renderer),
            surface: Some(surface),
            surface_format: Some(surface_format),
            player: None,
            movie_size,
            movie_texture: None,
            movie_texture_id: None,
            root_error,
            font_database,
            time: Instant::now(),
            last_pointer: PhysicalPosition::new(0.0, 0.0),
        })
    }

    fn start_game(&mut self) {
        if self.player.is_some() {
            return;
        }
        let window = self.window.clone().expect("window exists");
        let descriptors = self.descriptors.clone().expect("descriptors exist");
        let player = build_player(
            &window,
            &descriptors,
            &self.event_loop,
            self.font_database.clone(),
            self.movie_size.clone(),
            self.root_error.clone(),
        )
        .expect("player construction failed");
        self.player = Some(player);
        self.screen = Screen::Playing;
    }

    fn movie_rect(&self) -> egui::Rect {
        // Aspect-fit the movie into the central panel, used for both drawing
        // and window->movie coordinate mapping.
        let size = self.movie_size.lock().unwrap().unwrap_or((800, 600));
        let (mw, mh) = (size.0.max(1) as f32, size.1.max(1) as f32);
        let window_size = self.window.as_ref().expect("window").inner_size();
        let (ww, wh) = (window_size.width.max(1) as f32, window_size.height.max(1) as f32);
        let scale = (ww / mw).min(wh / mh);
        let w = mw * scale;
        let h = mh * scale;
        egui::Rect::from_min_size(
            egui::pos2((ww - w) / 2.0, (wh - h) / 2.0),
            egui::vec2(w, h),
        )
    }

    fn window_to_movie(&self, pos: PhysicalPosition<f64>) -> (f64, f64) {
        let rect = self.movie_rect();
        let size = self.movie_size.lock().unwrap().unwrap_or((800, 600));
        let x = (pos.x as f32 - rect.min.x) * size.0.max(1) as f32 / rect.width();
        let y = (pos.y as f32 - rect.min.y) * size.1.max(1) as f32 / rect.height();
        (x as f64, y as f64)
    }

    fn ensure_movie_texture(&mut self, renderer: &WgpuRenderBackend<TextureTarget>) {
        let device = &self.descriptors.as_ref().expect("descriptors").device;
        let texture = &renderer.descriptors(); // placeholder — see step notes
        // The TextureTarget's texture is accessible via the backend; the app
        // keeps `TextureTarget` alive inside the player's renderer, so here we
        // (re)register the target texture with egui when the movie size changes.
        let _ = (device, texture);
    }
}
```

**Implementation note for the renderer-texture plumbing:** the app renders the movie into a `TextureTarget` held by `WgpuRenderBackend` inside the player. The texture to draw is the one passed to `WgpuRenderBackend::new` — so `build_player` should hand back the `TextureTarget` it created, or the app should create the target itself and pass it in. **Preferred approach:** `build_player` returns `(Arc<Mutex<Player>>, TextureTarget)` and the app stores the `TextureTarget` (`movie_texture: Option<TextureTarget>`), registers its texture view with `egui_renderer.register_native_texture(&device, &view, wgpu::FilterMode::Linear)` when created or resized, and draws it with `egui::Image`. Replace `movie_texture: Option<wgpu::Texture>` with `movie_target: Option<TextureTarget>` + `movie_texture_id: Option<egui::TextureId>` accordingly (adjust Task 7's `build_player` signature to return the target; keep the placeholder rect `(800,600)` until metadata arrives, then rebuild the target + re-register).

- [ ] **Step 2: Event handling + render loop**

```rust
impl ApplicationHandler<RuffleEvent> for App {
    fn resumed(&mut self, _event_loop: &ActiveEventLoop) {}

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: RuffleEvent) {
        match event {
            RuffleEvent::TaskPoll(runnable) => runnable.run(),
        }
        self.window.as_ref().expect("window").request_redraw();
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let window = self.window.as_ref().expect("window");

        // Feed the event to egui first (it needs everything to drive its widgets).
        let egui_winit = self.egui_winit.as_mut().expect("egui state");
        egui_winit.on_window_event(window, &event);

        match &self.screen {
            Screen::Playing => {
                if let Some(player) = &self.player {
                    let mut player_lock = player.lock().unwrap();
                    match event {
                        WindowEvent::CursorMoved { position, .. } => {
                            if position == self.last_pointer {
                                return;
                            }
                            self.last_pointer = position;
                            let (x, y) = self.window_to_movie(position);
                            player_lock.handle_event(PlayerEvent::MouseMove { x, y, source: MouseInputSource::Mouse });
                        }
                        WindowEvent::MouseInput { button, state, .. } => {
                            let (x, y) = self.window_to_movie(self.last_pointer);
                            let button = match button {
                                winit::event::MouseButton::Left => MouseButton::Left,
                                winit::event::MouseButton::Right => MouseButton::Right,
                                winit::event::MouseButton::Middle => MouseButton::Middle,
                                _ => MouseButton::Unknown,
                            };
                            let event = match state {
                                ElementState::Pressed => PlayerEvent::MouseDown { x, y, button, index: None, source: MouseInputSource::Mouse },
                                ElementState::Released => PlayerEvent::MouseUp { x, y, button, source: MouseInputSource::Mouse },
                            };
                            player_lock.handle_event(event);
                        }
                        WindowEvent::MouseWheel { delta, .. } => {
                            let delta = match delta {
                                MouseScrollDelta::LineDelta(_, dy) => MouseWheelDelta::Lines(dy.into()),
                                MouseScrollDelta::PixelDelta(pos) => MouseWheelDelta::Pixels(pos.y),
                            };
                            player_lock.handle_event(PlayerEvent::MouseWheel { delta });
                        }
                        WindowEvent::CursorLeft { .. } => {
                            player_lock.handle_event(PlayerEvent::MouseLeave);
                        }
                        WindowEvent::Focused(true) => player_lock.handle_event(PlayerEvent::FocusGained),
                        WindowEvent::Focused(false) => player_lock.handle_event(PlayerEvent::FocusLost),
                        WindowEvent::KeyboardInput { event, .. } => {
                            let key = winit_input_to_ruffle_key_descriptor(&event);
                            match event.state {
                                ElementState::Pressed => {
                                    player_lock.handle_event(PlayerEvent::KeyDown { key });
                                    if let Some(control_code) = winit_to_ruffle_text_control(&event, self.modifiers) {
                                        player_lock.handle_event(PlayerEvent::TextControl { code: control_code });
                                    } else if let Some(text) = event.text {
                                        for codepoint in text.chars() {
                                            player_lock.handle_event(PlayerEvent::TextInput { codepoint });
                                        }
                                    }
                                }
                                ElementState::Released => {
                                    player_lock.handle_event(PlayerEvent::KeyUp { key });
                                }
                            }
                        }
                        WindowEvent::Ime(ime) => match ime {
                            Ime::Enabled => {}
                            Ime::Preedit(text, cursor) => {
                                player_lock.handle_event(PlayerEvent::Ime(ImeEvent::Preedit(text, cursor)));
                            }
                            Ime::Commit(text) => {
                                player_lock.handle_event(PlayerEvent::Ime(ImeEvent::Commit(text)));
                            }
                            Ime::Disabled => {}
                        },
                        _ => {}
                    }
                }
            }
            _ => {
                // Overlay screens: egui widgets handle their own clicks; the
                // actions are applied below in the egui pass.
            }
        }

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => {
                self.reconfigure_surface();
            }
            _ => {}
        }
        window.request_redraw();
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if matches!(self.screen, Screen::Playing) && self.player.is_some() {
            let new_time = Instant::now();
            let dt = FloatDuration::from_std(new_time.duration_since(self.time));
            self.time = new_time;
            let next_frame = self.player.as_ref().map(|player| {
                let mut player_lock = player.lock().unwrap();
                player_lock.tick(dt);
                new_time + player_lock.time_til_next_frame()
            });
            if let Some(next_frame) = next_frame {
                event_loop.set_control_flow(ControlFlow::WaitUntil(next_frame));
            }
            if self.player.as_ref().is_some_and(|player| player.lock().unwrap().needs_render()) {
                self.window.as_ref().expect("window").request_redraw();
            }
        } else {
            event_loop.set_control_flow(ControlFlow::Wait);
        }
    }
}
```

Notes: `self.modifiers` — track `WindowEvent::ModifiersChanged` and store it (add `modifiers: winit::keyboard::ModifiersState` to the struct, default). The key/control helpers `winit_input_to_ruffle_key_descriptor` / `winit_to_ruffle_text_control` must be copied from `/workspace/ruffle/desktop/src/util.rs:19-335` (they are pure functions; copy them into a small `src/input.rs` module with the helpers they reference — the file is mechanical). The player tick loop mirrors app.rs:362-410.

- [ ] **Step 3: The render method + screen widgets**

```rust
impl App {
    fn render(&mut self) {
        let window = self.window.as_ref().expect("window");
        let surface = self.surface.as_ref().expect("surface");
        let surface_texture = match surface.get_current_texture() {
            Ok(texture) => texture,
            Err(error) => match error {
                wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated => {
                    tracing::warn!("Surface became unavailable: {error:?}, reconfiguring");
                    self.reconfigure_surface();
                    return;
                }
                wgpu::SurfaceError::Timeout => {
                    tracing::warn!("Surface became unavailable: {error:?}, skipping frame");
                    return;
                }
                wgpu::SurfaceError::OutOfMemory | wgpu::SurfaceError::Other => {
                    panic!("wgpu: surface error: {error:?}");
                }
            },
        };

        // `Context` is cheaply cloneable (Arc-backed); cloning it lets the
        // `run` closure below borrow `self` mutably without fighting the
        // borrow of `self.egui_winit`.
        let egui_ctx = self
            .egui_winit
            .as_ref()
            .expect("egui state")
            .egui_ctx()
            .clone();
        let raw_input = self.egui_winit.as_mut().expect("egui state").take_egui_input(window);

        // If the root movie failed to download, show the error screen.
        if let Some(message) = self.root_error.lock().unwrap().take() {
            self.screen = Screen::Error { message };
        }

        let mut full_output = egui_ctx.run(raw_input, |ctx| {
                match &self.screen {
                    Screen::Disclaimer => { disclaimer_ui(ctx); }
                    Screen::Setup { sources } => {
                        if let Some(choice) = setup_ui(ctx, sources) {
                            self.apply_migration_choice(choice);
                        }
                    }
                    Screen::Error { message } => {
                        if error_ui(ctx, message, self.player.is_some()) {
                            self.screen = Screen::Playing;
                        }
                    }
                    Screen::Playing => {
                        if let Some(player) = &self.player {
                            let mut player_lock = player.lock().unwrap();
                            player_lock.render();
                        }
                        if let Some(texture_id) = self.movie_texture_id {
                            // Draw the movie letterboxed; egui::Image sized to
                            // the aspect-fit rect via `egui::Image::new(...).fit_to_exact_size(rect.size())` or an
                            // `egui::Area` placed at rect.min with the texture size.
                            // If the movie size is unknown yet (loading), draw a
                            // "Loading DragonFable…" label instead.
                            ctx.request_repaint();
                        }
                    }
                }
            });

        let clipped = egui_ctx.tessellate(full_output.shapes, full_output.pixels_per_point);
        let screen_descriptor = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [window.inner_size().width, window.inner_size().height],
            pixels_per_point: window.scale_factor() as f32,
        };
        let mut encoder = self.descriptors.as_ref().expect("descriptors")
            .device.create_command_encoder(&Default::default());
        let surface_view = surface_texture.texture.create_view(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("main pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &surface_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            self.egui_renderer.as_ref().expect("egui renderer")
                .render(&mut pass, &clipped, &screen_descriptor);
        }
        self.descriptors.as_ref().expect("descriptors").queue.submit([encoder.finish()]);
        surface_texture.present();
    }

    fn reconfigure_surface(&mut self) {
        let window = self.window.as_ref().expect("window");
        let size = window.inner_size();
        if size.width == 0 || size.height == 0 {
            return;
        }
        if let (Some(surface), Some(device), Some(format)) = (
            self.surface.as_ref(),
            Some(&self.descriptors.as_ref().expect("descriptors").device),
            self.surface_format,
        ) {
            surface.configure(
                device,
                &wgpu::SurfaceConfiguration {
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                    format,
                    width: size.width,
                    height: size.height,
                    present_mode: wgpu::PresentMode::AutoVsync,
                    desired_maximum_frame_latency: 2,
                    alpha_mode: wgpu::CompositeAlphaMode::Auto,
                    view_formats: Vec::new(),
                },
            );
        }
    }

    fn apply_migration_choice(&mut self, choice: Option<usize>) {
        let detected: Vec<MigratedSource> = match &self.screen {
            Screen::Setup { sources } => sources.clone(),
            _ => Vec::new(),
        };
        let source = choice.map(|index| detected[index].id.clone());
        let result = if let Some(index) = choice {
            migration::sources()
                .iter()
                .find(|s| s.id == detected[index].id)
                .and_then(|s| {
                    let dirs: Vec<_> = s
                        .roots
                        .iter()
                        .flat_map(|root| migration::detect(root, s.include_proxy_host))
                        .collect();
                    (!dirs.is_empty()).then(|| migration::copy_source(&dirs, &config::save_dir()))
                })
        } else {
            None
        };
        match result {
            Some(Ok(copied)) => tracing::info!("Copied {copied} save files from {source:?}"),
            Some(Err(error)) => tracing::warn!("Migration copy failed: {error}"),
            None => {}
        }
        self.state.migration = Some(crate::config::MigrationChoice {
            source,
            copied_at_unix: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0),
        });
        let _ = self.state.save(&config::config_dir());
        self.screen = Screen::Playing;
        self.start_game();
    }
}

// Pure egui widgets for the overlay screens. (These live in app.rs for now;
// they are the designated growth point for future settings UI.)
fn disclaimer_ui(ctx: &egui::Context) {
    egui::CentralPanel::default()
        .frame(egui::Frame::none().fill(egui::Color32::from_rgb(0x66, 0x00, 0x00)))
        .show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(120.0);
                ui.heading("Disclaimer");
                ui.add_space(24.0);
                ui.label(
                    "This is a 3rd party launcher that is not supported nor endorsed by Artix Entertainment.",
                );
                ui.label("By clicking 'Continue', you agree to use this launcher at your own risk.");
                ui.add_space(32.0);
                // Continue button handled by the caller via return value in the
                // real implementation; see the note below.
            });
        });
}
```

**Note on widget→action plumbing:** pure helper functions returning an action are cleaner than mutating `self` inside the egui closure (borrow conflicts). Implement them as: `fn disclaimer_ui(ctx) -> bool` (true = Continue clicked), `fn setup_ui(ctx, sources) -> Option<Option<usize>>` (`Some(None)` = "Don't copy", `Some(Some(i))` = copy from source i, `None` = nothing clicked), `fn error_ui(ctx, message) -> bool` (true = Retry clicked), and a `fn loading_ui(ctx)` for the pre-metadata state. Wire them in the `run` closure accordingly (`if disclaimer_ui(ctx) { self.state.disclaimer_accepted = true; let _ = self.state.save(...); self.screen = ...; }`).

- [ ] **Step 4: Real `src/main.rs`**

```rust
#![windows_subsystem = "windows"]

mod app;
mod config;
mod input;
mod migration;
mod navigator;
mod player;
mod ui;

use std::io::Write;
use tracing_subscriber::EnvFilter;

fn main() -> anyhow::Result<()> {
    init_logging();

    let event_loop = winit::event_loop::EventLoop::<crate::player::RuffleEvent>::new()?;
    let mut app = app::App::new(&event_loop)?;
    event_loop.run_app(&mut app)?;
    Ok(())
}

fn init_logging() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("warn,ruffle=info,dragonfable_cache=info"));

    let (file_writer, _guard) = tracing_appender::non_blocking(
        std::fs::File::create(config::log_dir().join("log.txt"))
            .expect("log file must be creatable"),
    );
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(file_writer)
        .with_ansi(false)
        .init();
}
```

(Keep the `_guard` alive for the program's lifetime — store it in a `static`/leak it with `std::mem::forget` or keep a `Box::leak`; simplest is `let _guard = ...; std::mem::forget(_guard);` with a comment that the worker must outlive main. Or log to stderr only on Linux and file on all platforms — the file is essential on Windows where there is no console.)

- [ ] **Step 5: Build and fix**

Run: `cargo build`
Expected: compiles. Expect a few iterations: borrow-checker issues around the egui closure (use the helper-function pattern above), exact `PlayerEvent` field names (cross-check `/workspace/ruffle/core/src/events.rs`), and the `register_native_texture`/`Image` API details (cross-check `egui_wgpu` docs).

- [ ] **Step 6: Manual smoke test**

Run: `cargo run`
Expected: window opens → disclaimer screen → Continue → (setup screen if save data found) → game loads from play.dragonfable.com → DragonFable plays; cache files under `~/.cache/dragonfable/` (Linux) start with `DFCACHE` magic bytes. On failure the error screen shows with Retry. Ctrl+C to quit (or add an Escape→exit shortcut if the game doesn't capture it).

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "Add app event loop, rendering, screens, and input forwarding"
```

---

### Task 9: Final polish and verification

**Files:**
- Modify: `src/app.rs` (if needed during polish)

- [ ] **Step 1: Panic handling**

Wrap the event loop body so a panic in rendering doesn't leave a dead window — mirror `/workspace/ruffle/desktop/src/main.rs:120-170` (`std::panic::catch_unwind` around the frame, logging the panic and continuing with an error screen). If that proves too invasive for winit 0.30's `run_app`, at minimum install a panic hook (`std::panic::set_hook`) that logs the panic to the file log, and document that behavior. Keep it simple; the file log is the important part.

- [ ] **Step 2: Full verification**

```bash
cd /workspace/df-cache-layer && cargo test && cargo build --release
cd /workspace/desktop && cargo test && cargo build --release
```

Expected: df-cache-layer: all unit + integration tests pass. desktop: all unit tests pass, release build succeeds.

- [ ] **Step 3: Manual end-to-end check**

Run the release binary; verify: disclaimer shows on first run (delete `~/.config/dragonfable/state.toml` to reset); setup screen appears when a fake Flash directory exists (create `~/.macromedia/Flash_Player/#SharedObjects/test123/play.dragonfable.com/x.sol` and confirm it is copied to `~/.local/share/dragonfable/SharedObjects/play.dragonfable.com/x.sol` only after choosing that source); game boots; cache files are `DFCACHE`-magic'd; second run skips both screens.

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "Polish: panic handling and verification"
```

---

## Self-Review Notes

- **Spec coverage:** design-doc sections map to tasks — cache encryption (Tasks 1-2), scaffold + deps (3), dirs/state (4), migration sources + mapping (5), screens + UiBackend (6), player/navigator wiring + 512MB cache + hardcoded URL (7), event loop/rendering/input/letterbox (8), error overlay + retry + logging + verification (9). Android keeps its cache-dir wiring untouched; it inherits the new format via the shared crate (Task 2). Non-goals (macOS, settings UI, CLI) are not implemented anywhere.
- **Type consistency:** `CacheFile`/`CacheWriter::finish` names match between Task 1 and Task 2; `build_player` signature in Task 7 matches Task 8's call (modulo the documented renderer-target return change, which both tasks note); `Screen`/`initial_screen`/`MigrationSource`/`State` signatures are consistent across Tasks 4-8.
