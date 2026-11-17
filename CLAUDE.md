# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

kasset is a keyboard-driven terminal music player (Rust, ratatui + crossterm UI, rodio audio, lofty tags) for a local music folder. Single binary crate, no workspace.

## Commands

```sh
cargo build                                   # needs libasound2-dev + pkg-config on Linux
cargo run -- [DIR]                            # open DIR (default: XDG music dir, ~/Music, then cwd)
cargo run -- --help                           # key summary lives in src/cli.rs
cargo test                                    # all unit tests (no sound hardware needed)
cargo test queue::                            # tests in one module
cargo test some_test_name                     # single test by name substring
cargo clippy --all-targets -- -D warnings     # must be clean; pedantic is on
cargo +1.89 build                             # MSRV check (rust-version = 1.89)
```

Whenever dependencies change, re-check `rust-version` against the highest dependency MSRV (`cargo metadata`).

## Lints and code rules

- `unsafe_code = "forbid"`; clippy `all` + `pedantic` warn, plus `unwrap_used`, `expect_used`, `dbg_macro`, `todo`. Tests avoid `unwrap()` too (e.g. `unwrap_or_else(|never| match never {})`).
- Edition 2024. Every module starts with a `//!` doc comment explaining its role and invariants — keep them accurate when changing behaviour.
- Tags, file names and playlist contents are **untrusted input**. All text that reaches the screen must go through `text::sanitize` / `text::display_path`; decoders and tag readers run inside `panic_guard::quietly` so their panics become errors without printing over the TUI.
- Release profile uses `panic = "unwind"` (required for `catch_unwind` in `panic_guard`).

## Architecture

Data flows `main.rs` → `cli::resolve_root` → `terminal::ensure_terminal` → `Player::open` (fail fast with no audio device) → `library::scan` → `App::new(tracks, player, FsStore, …)` → `terminal::run` event loop.

Layering, from pure logic outward:

- `queue.rs` — pure play order (shuffle, repeat off/all/one). Plays a list of **items** (library indices, duplicates allowed) addressed by item position. Supports live edits (`remove_item`, `move_item`, `append_items`, `replace_items`); removing the playing item leaves the queue *detached* (it moves on to the removed item's successor, never back).
- `player.rs` — the `Audio` trait plus the rodio-backed `Player`. Everything above it is generic over `A: Audio` so tests use fakes (`test_support::SilentAudio`; `FakeAudio` in `playback.rs` tests can end/fail individual tracks).
- `playback.rs` — `Playback<A>` ties `Queue` to `Audio`: transport commands, auto-advance, skipping unplayable tracks, `Notice` status messages. A `Source` (library or a playlist) defines the current play order; playlist edits/reloads/renames/deletes are forwarded so the playing order stays consistent.
- `playlist.rs` + `playlist/{name,paths,store}.rs` — `.m3u8` files in `<root>/Playlists`. A playlist is kept as **raw lines with their own line endings** so saves only touch edited lines (comments, BOM, CRLF, unmatched entries, missing final newline survive). Non-UTF-8 or oversized files are parsed but read-only. `paths::Resolver` matches entries literal → `\`-as-separator → `canonicalize` (bounded). `store::FsStore` implements the `PlaylistStore` trait: atomic temp-file + rename saves, stamp check to never overwrite external changes (`ChangedOnDisk`), no-clobber create/rename via `hard_link`, symlinked playlists stay links, but only if the link target resolves inside `Playlists/` (else `ReadOnlyReason::LinksOutside`, re-checked in `save`). kasset must never write outside `Playlists/`.
- `app.rs` — all UI state and key → action mapping, no terminal I/O; files only via `PlaylistStore`. Two sides switched with `Tab` (library / playlists; the playlists side shows either the list or one open playlist), each list with its own filter and selection. `Mode` = Normal / Search / Picker (add-to-playlist target) / NameInput (`NamePurpose::Create` for "New playlist…" or `NamePurpose::Rename` for `R`, prefilled with the current name) / ConfirmDelete (`D`: `y` deletes, any other key cancels). Entry edits (`d`, `J`/`K`) are made on a copy of the open playlist, saved, then installed, so a failed or conflicting save leaves the screen unchanged. Entry points: `App::handle_key`, `App::tick`.
- `ui.rs` — renders `App` read-only (only scroll state kept). Tested with ratatui `TestBackend`.
- `terminal.rs` — raw mode / alt screen setup and restore on every exit path (quit, error, signal, panic); redirects fd 2 to `/dev/null` while the UI is up so ALSA can't print over it. A second identical signal is an emergency exit.
- `library.rs` — recursive discovery (`walkdir`, skips unreadable dirs, broken links, loops; dedupes hard links by `(dev, ino)`), tag reading with file-name fallback, sorting.

## Testing conventions

- App behaviour is tested as **key sequences** against `App` with fake audio and a `tempfile` store; rendering via `TestBackend`. Shared fixtures (in-memory WAV, `track(...)`, `SilentAudio`, `no_store()`) are in `src/test_support.rs`.
- Forcing write failures must not use `chmod` (root ignores it): make `Playlists` a regular file, or pre-create a directory at the temp path `.<name>.m3u8.tmp-<pid>`. Permission and symlink tests are `#[cfg(unix)]`.
- Existing queue/playback tests must keep passing unchanged when generalising play-order logic.

## OpenSpec workflow

Features are specified and tracked with OpenSpec (`openspec/`, skills under `.claude/skills/`, commands `/opsx:propose|apply|update|sync|archive|explore`).

- Current specs: `openspec/specs/{audio-playback,music-library,player-tui}/spec.md`.
- Active change: `openspec/changes/add-playlists/` — `design.md` holds the numbered decisions (D1–D9) referenced in code and tasks; `tasks.md` tracks progress by task group. Mark tasks `[x]` as they are completed and verified.
- Archived changes live in `openspec/changes/archive/`.
- When adding keys, update the README keys tables, the `--help` summary in `src/cli.rs`, and the in-app key help bar together.
