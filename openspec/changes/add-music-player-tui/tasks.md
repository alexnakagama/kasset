# Tasks

## 1. Project setup

- [ ] 1.1 Create the `kasset` binary crate (`Cargo.toml`, `src/main.rs`, `.gitignore` with `target/`) and verify `cargo build` succeeds
- [ ] 1.2 Add dependencies (ratatui, crossterm, rodio with symphonia features, lofty, walkdir, clap derive, dirs, rand, anyhow) and verify `cargo build` succeeds on Linux with ALSA headers installed

## 2. CLI and library scanning

- [ ] 2.1 Implement `kasset [DIR]` argument parsing with `--help`/`--version` and library root resolution (arg → music dir → cwd, error on missing dir); verify `cargo run -- --help` prints usage plus the keybinding summary (`after_help`) and `cargo run -- /no/such/dir` exits non-zero with the folder named
- [ ] 2.2 Implement recursive scan with case-insensitive extension filter and skipping unreadable dirs; verify with unit tests using a temp dir (nested files found, `.jpg`/`.txt` ignored)
- [ ] 2.3 Implement metadata reading with lofty and filename/"Unknown" fallbacks; verify unit test that an untagged file gets its filename as title
- [ ] 2.4 Implement library sort (artist, album, track number, title, case-insensitive); verify unit test that tracks 1, 2, 10 sort numerically

## 3. Playback engine

- [ ] 3.1 Implement play-order queue (linear and Fisher–Yates shuffle with current track first) and repeat modes off/all/one; verify unit tests for next/prev, wrap with repeat all, replay with repeat one, stop at end with repeat off, and shuffle visiting every track once
- [ ] 3.2 Implement `Player` wrapping rodio output stream and sink: play, pause/resume, stop, seek ±5s clamped, volume 0–100% in 5% steps, position, finished detection; verify volume clamping by unit test and playback manually with a local mp3/flac
- [ ] 3.3 Handle no audio device at startup (clear message, non-zero exit) and decode/open errors (report + skip to next track, stopping if every track fails); verify by playing a deliberately corrupt file and seeing the error and auto-skip, including with repeat one

## 4. Terminal UI

- [ ] 4.1 Implement terminal setup/teardown with panic hook restoring the terminal; verify that `q`, Ctrl+C and a forced panic all leave the shell usable
- [ ] 4.2 Implement the event loop (250 ms poll, tick for auto-advance, resize redraw); verify elapsed time advances each second and auto-advance plays the next track
- [ ] 4.3 Render layout: track list with selected vs playing markers, now-playing panel, progress gauge with `m:ss / m:ss`, volume/shuffle/repeat indicators, keybinding bar, status message area, empty-library message; verify visually in a terminal and with a ratatui `TestBackend` render test for the empty-library message
- [ ] 4.4 Implement key handling for navigation (↑↓/jk, PgUp/PgDn, g/G/Home/End) and transport keys (Enter, Space, s, n, p, ←/→, +/-, z, r, q); verify unit tests on `App::handle_key` for selection movement and manually for transport keys
- [ ] 4.5 Implement `/` search mode with live case-insensitive filtering on title/artist/album, Enter to keep, Esc to clear; verify unit test that filtering "beat" keeps only matching tracks

## 5. Docs and integration

- [ ] 5.1 Write `README.md` with build prerequisites (`libasound2-dev`, `pkg-config`), `cargo install --path .`, usage, keybindings and known limitations (.opus unsupported); verify the install command puts `kasset` on PATH and `kasset --version` runs
- [ ] 5.2 End-to-end check: run `kasset` on a real music folder, exercise every keybinding from the spec, then run `cargo test` and `cargo clippy -- -D warnings` with no failures

## Workflow follow-up

- Archive the change with `/opsx:archive` once implementation is verified.
