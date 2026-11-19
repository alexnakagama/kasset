# Tasks

Every group ends green: `cargo test` (the whole suite) and `cargo clippy --all-targets -- -D warnings` pass before the group is handed off for review. Each group keeps the `//!` docs of the modules it touches, and its line in `CLAUDE.md`'s Architecture section, accurate.

## 1. LRC parsing (new `src/lyrics.rs`)

- [x] 1.1 Add `src/lyrics.rs` with a `//!` doc stating its role and invariants: pure, no I/O, every text sanitised, synced lines sorted. Implement `Lyrics::{Synced, Plain}`, `Line`, `parse`, `current` and `next_change` as in D1, with `MAX_LINES` = 10 000. Register the module in `main.rs`. Verify with unit tests:
  - `[mm:ss]`, `[mm:ss.x]`, `[mm:ss.xx]` and `[mm:ss.xxx]` parse to the right durations, including minutes ≥ 100; `[01:75.00]` and other malformed stamps are not timestamps;
  - `[00:12.00][01:30.00]Chorus` gives two lines (spec *Several timestamps*);
  - out-of-order lines are sorted, and equal times keep file order (spec *Lines out of order*);
  - `[offset:+500]` and `[offset:-500]` shift times, saturating at zero (spec *Offset*);
  - `[ar:Band]` and other `[tag:value]` headers are dropped, and `<mm:ss.xx>` word timings are removed (spec *Header and word timings*);
  - `[Chorus]` and `[Verse 1] la la` stay as text in plain lyrics (spec *Section markers*);
  - a blank timed line becomes an empty line that is current from its time (spec *Blank timed line*), and a trailing blank timed line is kept, not trimmed (spec *Blank last line*);
  - with any timestamp, untimed lines are dropped; with none, the result is `Plain` in file order, with blank edges trimmed (spec *Plain text lyrics*);
  - `[99999999999999999999:00.00]x`, a 7-digit-minute stamp and `[offset:99999999999999999999]` neither panic nor wrap; the offset is clamped to ±1 h (spec *Absurd timestamps*);
  - an ESC sequence in a line is sanitised (spec *Escape sequences*);
  - empty input, headers only and whitespace only give `None`;
  - `current` returns `None` before the first line, the last line at or before the position otherwise, and the last line after the end; `next_change` returns the next later time and `None` after the last;
  - more than `MAX_LINES` lines are capped.

  Add the `lyrics.rs` line to `CLAUDE.md`'s Architecture section.

## 2. Loading lyrics (new `src/bounded_read.rs`, new `src/lyrics/source.rs`, `src/playlist/store.rs`, `src/library.rs`)

- [x] 2.1 Move `open_regular`, `read_limited` and `not_a_file` from `playlist/store.rs` into a new `src/bounded_read.rs` (with a `//!` doc covering the FIFO-safe, size-capped, regular-files-only rules). `read_limited` returns the file's `Metadata`; the store builds its `FileStamp` from it (D3). Mention `bounded_read.rs` in `CLAUDE.md` next to the playlist store line. Verify that every existing `playlist` and store test passes unchanged, and that clippy is clean on unix and with `--target x86_64-pc-windows-gnu` if that target is installed (otherwise rely on CI).
- [x] 2.2 Give `read_tagged` a `ParseOptions` parameter (the scan passes its current options unchanged). Add `library::lyrics_tag(path) -> Option<String>`, returning the raw `ItemKey::Lyrics` or `ItemKey::UnsyncLyrics` text, primary tag first, parsed with `.read_cover_art(false).read_properties(false)` (D2). Existing `library` tests must pass unchanged. Verify with tests that write tags with lofty into temp copies of the in-memory WAV, or of minimal FLAC/MP4/MP3 files if WAV lacks a fitting tag type:
  - Vorbis-comment `LYRICS` round-trips with its newlines intact;
  - ID3v2 `USLT` round-trips;
  - a file without lyrics gives `None`;
  - a garbage file gives `None` without panicking.

  If a format cannot round-trip through lofty, record it in the design's Risks and the README instead of working around it.
- [x] 2.3 Add `src/lyrics/source.rs` with `load(track) -> Option<Lyrics>`. It finds the sidecar (`lrc`, then `LRC`, then a bounded case-insensitive directory scan), reads it with `bounded_read` (1 MiB `MAX_LYRICS_BYTES`), decodes it (UTF-8 BOM, UTF-16 LE/BE BOM, else lossy), falls back to `lyrics_tag`, and runs everything inside `panic_guard::quietly` (D2). Update the `lyrics.rs` `//!` doc to mention the submodule. Verify with `tempfile` tests:
  - `song.lrc` is found (spec *Sidecar file*), and so are `song.LRC` and `song.Lrc` (spec *Upper-case extension*);
  - the sidecar wins over tag lyrics (spec *Sidecar wins over tags*);
  - with no sidecar, tag lyrics are used (spec *Tag lyrics*);
  - an empty sidecar gives `None`, with no tag fallback (spec *Empty sidecar*);
  - a directory named `song.lrc` is ignored, and so is a sidecar that cannot be opened: use a dangling symlink under `#[cfg(unix)]`, never `chmod` (spec *Unreadable sidecar*);
  - a Latin-1 file shows replacement characters, and a UTF-16 LE file with a BOM decodes correctly (spec *Not UTF-8*, *UTF-16 file*);
  - a 2 MiB file is read up to 1 MiB;
  - `#[cfg(unix)]`: a FIFO named `song.lrc` returns promptly and falls back (spec *FIFO instead of a file*).

  Describe sidecar and tag loading in the `lyrics.rs` line of `CLAUDE.md`.

## 3. App state and the `l` key (`src/app.rs`, `src/keys.rs`, `README.md`)

- [x] 3.1 Add `LyricsPane` to `App` (D4) with `on` (initially `true`), `fits`, `loaded_for` and `lyrics`. Add `set_lyrics_layout`, `lyrics_on`, `lyrics_shown`, `lyrics()`, `lyrics_ready()` and a `#[cfg(test)] set_lyrics_on(bool)` that raises no notice. In `tick`, load lyrics for the current track's path when the pane is shown and the path changed. Clear `loaded_for` and `lyrics` whenever `on` or `fits` becomes false. Add `lyrics_deadline(now)` (D7, minimum wait `LYRICS_MIN_WAIT` = 10 ms). Update the `app.rs` `//!` doc and its `CLAUDE.md` line (`LyricsPane`, `l`). Verify with key-sequence tests, using `tempfile` tracks with sidecars and fake audio:
  - lyrics load when a track starts;
  - seeking with Left/Right does not reload; check this by deleting the sidecar after the first load and seeing the lyrics stay;
  - `n` to a track without lyrics gives `None` (spec *Next track*);
  - nothing is loaded while `fits` is false, and the lyrics load on the first tick after `fits` becomes true; `lyrics_ready()` is false until then;
  - hiding the pane, rewriting the sidecar, then showing the pane loads the new text (spec *Edited while hidden*);
  - `lyrics_deadline` is `None` while paused, stopped, hidden or plain, equals the time until the next line while playing, and is never under 10 ms.
- [x] 3.2 Handle `l` in Normal mode from either pane: toggle `on`, and while `!fits` raise `Notice::info` with "Lyrics on: needs a terminal at least 100 columns wide" or "Lyrics off". Verify:
  - `l` `l` toggles off and on (spec *Hide and show*, state level);
  - from off, `l` with `fits` false gives the "on" notice (spec *Turned on while too narrow*);
  - from on, it gives the "off" notice (spec *Turned off while too narrow*);
  - with `fits` true there is no notice;
  - `/` then `l` adds `l` to the query (spec *Not while typing*);
  - `l` in the help overlay, the picker, the name input and the delete confirmation keeps those modes' existing behaviour.
- [x] 3.3 Add the `l` entry ("show / hide the lyrics", `bar: &[]`) to the "Display" group in `keys::GROUPS`, after `v` (D8). Add the same row to the README keys table, plus a short "Lyrics" section in the README: where `.lrc` files go, tag fallback, the empty-sidecar rule, and the 100-column minimum. Verify:
  - the existing `keys.rs` tests pass, and every bar is unchanged;
  - a new test shows `help_text()` and `help_lines()` contain `l` and "show / hide the lyrics" (spec *Listed in the help*);
  - `cargo run -- --help` lists it.

## 4. Lyrics pane and frame timing (`src/ui.rs`, `src/terminal.rs`, `CLAUDE.md`)

- [x] 4.1 In `render_body`, add the third column when `app.lyrics_on()` and the width is at least `LYRICS_MIN_WIDTH` (100), with width `(body/3).clamp(28, 48)`. Set `View::lyrics_fits` every frame, and feed it back through `terminal::event_loop` with `set_lyrics_layout` (D5). Update the `ui.rs` and `terminal.rs` `//!` docs. Verify with `TestBackend` tests:
  - at 120 columns there are three panes, left to right, with " Lyrics " titled (spec *Pane shown on a wide terminal*, *Lyrics pane visible*);
  - at 90 columns there are two panes (spec *Too narrow*);
  - at 100 and 200 columns the pane is 33 and 48 columns wide (spec *Width bounds*);
  - resizing 120 → 90 → 120 hides the pane and shows it again with `on` unchanged (spec *Narrowing the terminal*);
  - at 120×30 with the visualizer on, the pane ends above a full-width strip (spec *Spectrum below*);
  - `Tab` `Tab` never focuses it, and its border is unfocused (spec *Tab skips the pane*);
  - `l` hides it and the main pane widens (spec *Hide and show*);
  - after widening 90 → 120 with a playing track that has lyrics, no frame shows "No lyrics" before the lyrics (spec *Widening with lyrics*);
  - every existing `ui.rs` test passes. The fixtures `library()`, `with_playlists()` and `many()` call `set_lyrics_on(false)`, so the two-pane geometry (`MAIN_X`, title elision, table columns) is unchanged. Only the new tests turn the pane on, never with an `l` key press that could raise a notice. Any test that needed another edit is listed in the review hand-off.
- [x] 4.2 Add `lyric_rows` and the lyrics renderer (D6): wrap by display width, keep the synced current line in the middle, scroll plain lyrics by progress, apply `PLAYING`/`DIM`/default styles, and show "Nothing playing" and "No lyrics" through `render_empty`. Verify with pure tests of `lyric_rows` and `TestBackend` tests:
  - the current line is highlighted near the middle and earlier lines are dimmed (spec *Current line highlighted*);
  - before the first line nothing is highlighted and the top is shown (spec *Before the first line*);
  - after a seek, the new line is highlighted on the next frame (spec *Seeking*);
  - while paused the highlight stays (spec *Paused*);
  - a long line wraps onto several highlighted rows, and a word wider than the pane is broken (spec *Long line*);
  - plain lyrics three times the pane's height show their middle at half-way (spec *Scroll with progress*), and the top when the duration is unknown or zero; `top <= span` always, computed in `u128` milliseconds with no float casts;
  - "No lyrics" is shown and the side panes keep their widths (spec *Track without lyrics*), and "Nothing playing" is shown at start (spec *Nothing played yet*);
  - wide (CJK) characters wrap by display width.
- [x] 4.3 Give `terminal::next_deadline` the lyric deadline and return the earlier of the two (D7). Verify:
  - the existing `next_deadline` tests pass with `None`;
  - a new test shows a lyric deadline earlier than `FRAME` wins, and a later one does not.

  Update `CLAUDE.md`: the third column and `lyrics_fits` in the `ui.rs` line, and the lyric deadline in the `terminal.rs` line.

## 5. Integration checks

- [x] 5.1 Run kasset against a real folder with:
  - a FLAC with a sidecar `.lrc`;
  - a FLAC with `LYRICS` in its tags only;
  - an MP3 with `USLT`;
  - a track without lyrics;
  - a `.lrc` with `[offset:]` and word timings.

  Check by eye that lines change in time with the vocals, seeking follows, `l` and `v` combine at 100, 120 and 200 columns, and resizing below 100 and back works. Record anything off as a fix task or a design note.
- [x] 5.2 Run `cargo clippy --all-targets -- -D warnings`, `cargo test` and `cargo +1.89 build`; all must be clean. Confirm `Cargo.toml` and `Cargo.lock` are unchanged (no new dependencies), so the MSRV is unaffected.

## Workflow follow-up

- Sync the delta specs and archive the change after review (`/opsx:archive`).
