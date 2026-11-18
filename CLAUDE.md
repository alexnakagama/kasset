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

CI (`.github/workflows/ci.yml`) runs clippy + tests on Linux, macOS and Windows plus an MSRV build. Releases: bump `version` in `Cargo.toml`, dry-run `gh workflow run Release --ref master` (also runs on PRs touching Cargo.toml/Cargo.lock/.cargo/release.yml; builds + packages, never publishes), then push tag `vX.Y.Z`; `release.yml` checks the tag matches, tests, builds 5 targets and publishes a GitHub Release. Code must stay warning-free on non-unix too (gate unix-only test helpers with `#[cfg(unix)]`). `.cargo/config.toml` links the MSVC C runtime statically so `kasset.exe` needs no VC++ Redistributable.

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
- `app.rs` — all UI state and key → action mapping, no terminal I/O; files only via `PlaylistStore`. `Focus` = Sidebar / Main, moved with `Tab`/`BackTab`; `view()` (the list keys act on) is derived from `(focus, open)`: the sidebar, the library, or the open playlist. The sidebar `ListState` has a fixed "Library" row at value 0 (never filtered out; playlist `i` is value `i + 1` — go through `selected_playlist()` / `sidebar_value_of()`). `open` is set only by `Enter` on a sidebar playlist and cleared by `Enter` on "Library", deleting it, or its file vanishing; renaming it reloads it under the new name. Playlists are listed in `App::new` and on every sidebar focus; the app's own edits patch the sidebar entry with `update_info`. Every status message goes through `App::notify` (within one key press an error is never hidden; same-level notices are joined). Each list keeps its own filter and selection. `Mode` = Normal / Search / Picker (add-to-playlist target) / NameInput (`NamePurpose::Create` for "New playlist…" or `NamePurpose::Rename` for `R`, prefilled with the current name) / ConfirmDelete (`D`: `y` deletes, any other key cancels) / Help (`?`: scroll keys move it, clamped to `keys::help_line_count() - help_rows`, where `help_rows` is fed from `ui::View` by `terminal::run` like `page_size`; any other key closes it and is consumed). Entry edits (`d`, `J`/`K`) are made on a copy of the open playlist, saved, then installed, so a failed or conflicting save leaves the screen unchanged. Entry points: `App::handle_key`, `App::tick`.
- `ui.rs` — renders `App` read-only. Body = sidebar + main pane at ≥ `TWO_PANE_MIN_WIDTH` (70) columns, else only the focused pane full width; draws from `sidebar_list()` / `main_list()`, never `visible()`/`selected()` (those are the focused list only). `ui::View` keeps `sidebar_offset` / `main_offset` (the sidebar's offset counts playlist rows; "Library" and its separator stay on top) and feeds back `page_size` (focused pane) and `help_rows`. Sidebar marker comes from `App::sidebar_playing()` (only while a track is loaded). Popups centre over the body. All styles come from `theme.rs` — no `Color`/`Modifier`/`Stylize` in `ui.rs` outside tests. Tested with ratatui `TestBackend`.
- `keys.rs` — the single key table `GROUPS` (D7) feeding `--help` (`help_text`), the help overlay (`help_lines`) and the key bar (`bar(BarContext)`).
- `theme.rs` — style roles (D8): `PLAYING`, `SELECTED_FOCUSED`/`SELECTED_UNFOCUSED`, `BORDER_*`, `DIM`, `ERROR`, … Named ANSI colours only; each colour has one meaning.
- `terminal.rs` — raw mode / alt screen setup and restore on every exit path (quit, error, signal, panic); redirects fd 2 to `/dev/null` while the UI is up so ALSA can't print over it. A second identical signal is an emergency exit.
- `library.rs` — recursive discovery (`walkdir`, skips unreadable dirs, broken links, loops; dedupes hard links by `(dev, ino)`), tag reading with file-name fallback, sorting.

## Testing conventions

- App behaviour is tested as **key sequences** against `App` with fake audio and a `tempfile` store; rendering via `TestBackend`. Shared fixtures (in-memory WAV, `track(...)`, `SilentAudio`, `no_store()`) are in `src/test_support.rs`.
- Forcing write failures must not use `chmod` (root ignores it): make `Playlists` a regular file, or pre-create a directory at the temp path `.<name>.m3u8.tmp-<pid>`. Permission and symlink tests are `#[cfg(unix)]`.
- Existing queue/playback tests must keep passing unchanged when generalising play-order logic.

## OpenSpec workflow

Features are specified and tracked with OpenSpec (`openspec/`, skills under `.claude/skills/`, commands `/opsx:propose|apply|update|sync|archive|explore`).

- Current specs: `openspec/specs/{audio-playback,music-library,player-tui,playlists}/spec.md`.
- No active change. The playlists design decisions (D1–D9) referenced in code live in `openspec/changes/archive/2026-11-17-add-playlists/design.md`. In a change, `tasks.md` tracks progress by task group; mark tasks `[x]` as they are completed and verified.
- Archived changes live in `openspec/changes/archive/`.
- When adding or changing keys, update `keys::GROUPS` in `src/keys.rs` (it feeds `--help`, the help overlay and the key bar) and the README keys tables together.
