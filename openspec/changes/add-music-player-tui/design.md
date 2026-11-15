# Design

## Context

Greenfield Rust project (empty repo besides OpenSpec). Target platform is Linux first (the developer's machine); the stack chosen is cross-platform so macOS/Windows should work without extra design. Requirements are in `specs/` (music-library, audio-playback, player-tui).

## Goals / Non-Goals

**Goals:**
- Single binary `kasset`, no runtime services required.
- Responsive UI: audio never blocks rendering or input.
- Small, testable modules: library scanning and queue/order logic are pure and unit-tested without audio hardware.

**Non-Goals:**
- Gapless playback, crossfade, equalizer.
- Caching the library scan between runs (scan on startup is acceptable for typical libraries of a few thousand files).
- Async runtime (tokio) — not needed.

## Decisions

**TUI: `ratatui` + `crossterm`.** De facto standard, actively maintained, crossterm works on all platforms. Alternative `cursive` is higher-level but less flexible for custom layouts.

**Audio: `rodio` (symphonia decoders enabled).** `rodio` gives a `Sink` with pause/resume/volume/`try_seek`/`get_pos`, and symphonia is pure Rust (mp3, flac, vorbis, wav, aac/m4a). Alternatives: raw `cpal` + `symphonia` (more control, much more code), `libmpv` bindings (needs system lib). Opus is not decoded by symphonia, so `.opus` is excluded from the supported extensions rather than adding a C dependency now.

**Metadata: `lofty`.** Reads ID3, Vorbis comments, MP4 atoms and gives duration from properties, so duration is known before playback (needed for the progress bar). Alternative: symphonia probe — fewer tag formats normalized.

**Scanning: `walkdir`** with `follow_links(true)` and errors skipped. Metadata is read in the same pass; for large libraries this could be parallelised later with `rayon` without changing behavior.

**CLI: `clap` (derive).** Gives `--help`/`--version` for free; the keybinding summary is added via `after_help`. Music dir from `dirs::audio_dir()` (honours `XDG_MUSIC_DIR`), else `~/Music`, else cwd.

**Architecture: single-threaded event loop on the main thread.**
```
main ─► cli::parse ─► library::scan(root) ─► App::new(tracks, Player)
loop:
  terminal.draw(ui::render(&app))
  if crossterm::event::poll(250ms) → app.handle_key(key)
  app.tick()   // detects sink.empty() → advance per repeat mode
```
`rodio` already plays on its own audio thread, so the main thread only issues commands; no channels needed. A 250 ms poll satisfies the ≥2 Hz progress refresh.

**Modules:**
- `cli.rs` — `clap` argument struct and library root resolution.
- `library.rs` — `Track { path, title, artist, album, track_no, duration }`, `scan(root) -> Vec<Track>`, sorting.
- `queue.rs` — play order (linear or shuffled permutation), current index, `next/prev` with repeat mode. Pure logic, unit-tested.
- `player.rs` — wraps `OutputStream` + `Sink`: `play(path)`, `toggle_pause`, `stop`, `seek(delta)`, `set_volume`, `position`, `is_finished`. Elapsed time comes from `sink.get_pos()`.
- `app.rs` — UI state (selection, filter text, input mode, status message with expiry) and key → action mapping.
- `ui.rs` — rendering only, no state mutation.
- `main.rs` — setup/teardown, panic hook that restores the terminal.

**Shuffle:** Fisher–Yates permutation of indices into the full library, with the current track moved to the front so toggling shuffle doesn't jump. Filtering affects only the visible list, not the play order.

**Keybindings** (from specs): Enter play, Space pause, `s` stop, `n`/`p` next/prev, ←/→ seek ±5s, `+`/`=` and `-` volume, `z` shuffle, `r` repeat, `/` search, Esc clear, `q`/Ctrl+C quit, ↑↓/jk, PgUp/PgDn, g/G/Home/End.

## Risks / Trade-offs

- [Seeking unsupported for some formats/decoders in `rodio`] → `try_seek` errors are reported in the status bar instead of crashing.
- [Build needs ALSA headers on Linux] → document `sudo apt install libasound2-dev pkg-config` in README.
- [Large libraries make startup slow] → acceptable for MVP; show a "Scanning…" message before entering the loop; rayon/caching as follow-up.
- [`sink.get_pos()` drifts slightly after seek on some codecs] → cosmetic only; clamp to duration.
- [`.opus` not playable] → files are not scanned; documented limitation.

- [Skip-on-error could loop forever, e.g. repeat one or all with only corrupt files] → a failed track always advances as if repeat were off/all-wrapping once, and playback stops after every track in the order has failed consecutively.

## Migration Plan

New project; install with `cargo install --path .`, which places `kasset` in `~/.cargo/bin`. Rollback: `cargo uninstall kasset`.
