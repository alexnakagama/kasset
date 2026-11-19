# Design

## Context

See `proposal.md` for the motivation, and `specs/lyrics/spec.md` and `specs/player-tui/spec.md` for the required behaviour.

The parts of the current code that shape the approach:

- **Body layout.** `ui::render` cuts the spectrum strip off the bottom of the body *before* calling `render_body`, which splits the rest into sidebar (`body/4`, clamped to `SIDEBAR_WIDTH` 22..32) and main pane, or shows only the focused pane below `TWO_PANE_MIN_WIDTH` (70). A third column added inside `render_body` therefore sits above the strip, and the strip stays full width with no extra work.
- **Feedback from drawing.** Sizes known only while drawing reach `App` through `ui::View` and `terminal::event_loop` (`set_page_size`, `set_help_rows`, `set_spectrum_layout`). The visualizer's `on`/`fits` pair and its `v` notices are the model for the lyrics toggle.
- **Playback.** `Playback::current_track()` gives the current `Track` (with its raw `path`), `position()` is clamped to the duration, and `duration()` prefers the decoder. Seeking is a reload of the same track.
- **Frame timing.** `terminal::next_deadline(frame_start, animating)` returns `FRAME` (250 ms) or `ANIMATION_FRAME` (33 ms) after the frame start.
- **Untrusted input.** `text::sanitize` cleans one line (control characters become spaces, whitespace collapses, capped at `MAX_TEXT_CHARS` = 256) and returns `None` for blank text. `playlist/store.rs` already has a private, FIFO-safe, size-capped reader (`open_regular` + `read_limited`). `library::read_tagged` parses tags with lofty inside `panic_guard::quietly`, without cover art.
- **lofty 0.25** maps Vorbis `LYRICS` and MP4 `©lyr` to `ItemKey::Lyrics`, and ID3v2 `USLT` to `ItemKey::UnsyncLyrics`. It does not map `SYLT`.

## Goals / Non-Goals

**Goals:**
- Keep LRC parsing and line lookup a pure, deterministic module, testable like `queue.rs` and `visualizer.rs`.
- Read lyrics once per track, not per frame and not during the scan.
- Change the highlighted line on time without redrawing faster than today when nothing moves.
- No new dependencies.

**Non-Goals:**
- Noticing a `.lrc` file created or edited while its track is current and the pane is shown; it is picked up the next time the track becomes current or the pane is shown again.
- Word-by-word highlighting from enhanced LRC; word timings are only stripped.
- Reading lyrics off the UI thread (see Risks).
- `SYLT` frames, lyrics download, editing.

## Decisions

### D1. A pure `lyrics` module

`src/lyrics.rs` holds:

```
enum Lyrics {
    Synced(Vec<Line>),   // sorted by `at`, stable for equal times
    Plain(Vec<String>),  // file order
}
struct Line { at: Duration, text: String }

fn parse(text: &str) -> Option<Lyrics>
impl Lyrics {
    fn current(&self, position: Duration) -> Option<usize>     // Synced: last line with at <= position
    fn next_change(&self, position: Duration) -> Option<Duration> // Synced: first `at` > position
}
```

Parsing rules:
- A line is split into its leading `[...]` groups. `[mm:ss]`, `[mm:ss.x]`, `[mm:ss.xx]`, `[mm:ss.xxx]` (minutes of 1 to 6 digits, seconds below 60) are timestamps; a stamp with more minute digits is not a timestamp. `[offset:±N]` sets the offset. A leading group that is not a timestamp is a header only if it has the form `[tag:value]` (it contains `:`); a line made only of header groups is dropped. A bracket group without `:` (such as `[Chorus]` or `[Verse 1]`, common in tag lyrics) is ordinary text and stays in the line.
- All time arithmetic uses `checked_mul` / `saturating_add` / `saturating_sub`. The `[offset:]` value is parsed as `i64` and clamped to ±3 600 000 ms (one hour); an unparsable value is ignored. Crafted stamps can therefore never panic or wrap.
- The rest of the line, with `<mm:ss.xx>` word timings removed, is the text. It goes through `text::sanitize`; `None` (blank) is kept as an empty string, so a blank timed line ends the previous line's highlight (spec *Blank timed line*).
- A line with several timestamps produces one `Line` per timestamp.
- If at least one timestamp was found, the result is `Synced`, and lines without a timestamp are dropped (they are usually stray headers or credits). Otherwise it is `Plain`, with every non-header line in order. For `Plain` lyrics, leading and trailing blank lines are trimmed. For `Synced` lyrics no line is trimmed, because a blank timed line ends the previous line (a final `[03:40.00]` stops the last line's highlight for the outro). If no non-blank line remains, `parse` returns `None`.
- The offset is applied after parsing: `at - offset`, saturating at zero (a positive offset shows lines earlier, as in the LRC convention).
- At most `MAX_LINES` (10 000) lines are kept.
- `sort_by_key` (stable) orders synced lines, so lines with the same time keep file order.

`current` and `next_change` use `partition_point`, O(log n).

- *Alternative: an LRC crate (`lrc`, `lyric_parser`).* Rejected. The format is small, the crates are little-used, and we need specific leniency (headers, word timings, sanitising) anyway.

### D2. Loading: sidecar first, then tags (`src/lyrics/source.rs`)

`fn load(track: &Path) -> Option<Lyrics>`:

1. **Sidecar.** Try `track.with_extension("lrc")`, then `track.with_extension("LRC")`. If neither is a regular file, list the parent directory, stopping after 10 000 entries, for a file whose name equals the track's stem plus a `.lrc` extension compared case-insensitively. The first candidate that reads successfully is used; a candidate that cannot be read (permission denied, I/O error, not a regular file) is treated as absent. A readable sidecar is used even if it parses to `None`; there is no fallback to the tags then. An empty `.lrc` file therefore deliberately hides tag lyrics.
2. **Tags.** Otherwise `library::lyrics_tag(path) -> Option<String>`. It reuses `read_tagged` (which gains a `ParseOptions` parameter; the scan keeps its current options) and `ordered_tags`, and takes the first of `ItemKey::Lyrics`, then `ItemKey::UnsyncLyrics` (which also covers the Vorbis `UNSYNCEDLYRICS` key), from the primary tag first. It parses with `.read_cover_art(false).read_properties(false)`, so no audio properties are decoded on the UI thread. The **raw** string is returned, not sanitised, because `sanitize` would collapse the newlines; each line is sanitised by `parse`.
3. **Decoding the sidecar.** A UTF-8 BOM is stripped. A UTF-16 LE/BE BOM is decoded with `char::decode_utf16`, with replacement characters. Anything else goes through `String::from_utf8_lossy`.

The whole of `load` runs inside `panic_guard::quietly`, so a panic anywhere becomes "no lyrics".

- *Alternative: read lyrics during `library::scan` and keep them in `Track`.* Rejected. It reads one extra file per track at startup and keeps every track's lyrics in memory for the session.
- *Alternative: only `with_extension("lrc")`.* Rejected. On case-sensitive filesystems, `.LRC` files written by Windows tools would be missed. The directory listing runs only when both direct probes miss, and only once per track change.

### D3. One shared bounded reader (`src/bounded_read.rs`)

`open_regular` (non-blocking `O_RDONLY|O_NONBLOCK|O_NOCTTY|O_CLOEXEC` on unix) and `read_limited` move out of `playlist/store.rs` into `pub(crate)` functions in a new module. The new `read_limited` returns `(Vec<u8>, truncated, Metadata)`, and the store builds its `FileStamp` from the `Metadata` as before. Store behaviour and tests stay unchanged. Lyrics read with a 1 MiB limit (`MAX_LYRICS_BYTES`), so a truncated file shows what was read.

- *Alternative: a second copy in the lyrics module.* Rejected. The FIFO and size-cap rules are security-relevant and should live in one place.

### D4. App state: `LyricsPane`

```
struct LyricsPane {
    on: bool,                      // true at start; `l` toggles
    fits: bool,                    // fed back by the last frame
    loaded_for: Option<PathBuf>,   // track the cache belongs to
    lyrics: Option<Lyrics>,        // None: that track has no lyrics
}
```

- In `App::tick`, after `playback.tick()`, when `on && fits` and the current track's path differs from `loaded_for`, call `lyrics::load` and record the path. Keying on the **path** means seeking, pausing and replaying the same track never re-read (spec *Lyrics sources*). Showing the same file twice in a row from a playlist also reuses the cache, which is correct.
- While the pane is not shown, nothing is loaded, and `loaded_for` and `lyrics` are cleared whenever `on` or `fits` becomes false. Showing the pane again (with `l` or by widening the terminal) therefore always reloads, so a `.lrc` edited while the pane was hidden is picked up.
- `lyrics_ready()` is true when `loaded_for` equals the current track's path. Until then the renderer draws the pane empty, without a message. This covers the frame after a widen: `fits` is fed back after `draw`, so the next `draw` can come before the `tick` that loads. Without this, "No lyrics" would flash for up to 250 ms before the real lyrics appear.
- When no track is current, the UI shows "Nothing playing" from `playback.current_track() == None`.
- `set_lyrics_layout(fits)`, `lyrics_on()`, `lyrics()`, `lyrics_ready()`, and `lyrics_shown() = on && fits`. A `#[cfg(test)] set_lyrics_on(bool)` sets `on` without a notice, for test fixtures.
- `l` in Normal mode, from either pane, toggles `on`. While `!fits` it raises `Notice::info`: "Lyrics on: needs a terminal at least 100 columns wide" or "Lyrics off". It is the same pattern as `v` (`toggle_visualizer`), and the messages mirror its wording.

- *Alternative: load in `Playback` when a track loads.* Rejected. `Playback` is about audio and the queue, and loading only while the pane is shown is a UI concern.

### D5. Layout: a third column in `render_body`

```
 width >= 100 && lyrics_on:
 +----------------+-----------------------+----------------+
 | sidebar        | main (Fill)           | lyrics         |
 | body/4, 22..32 |                       | body/3, 28..48 |
 +----------------+-----------------------+----------------+
 width 70..99 or lyrics off:   sidebar | main      (as today)
 width < 70:                   focused pane only  (as today)
```

- `LYRICS_MIN_WIDTH = 100` and `LYRICS_WIDTH = (28, 48)` sit in `ui.rs` next to `TWO_PANE_MIN_WIDTH` and `SIDEBAR_WIDTH`. At 100 columns this gives 25 | 42 | 33, so the main pane always keeps at least 40 columns.
- `View` gains `lyrics_fits`, set every frame as `area.width >= LYRICS_MIN_WIDTH`, independent of `on` (like `spectrum_fits`). `terminal::event_loop` passes it to `set_lyrics_layout`.
- The border always uses `theme::border(false)`. The title is " Lyrics ".
- The focused pane's `page_size` is unaffected because the height is unchanged.

### D6. Rendering lyrics

A pure function in `ui.rs` turns lyrics and a width into rows:

```
fn lyric_rows(lyrics: &Lyrics, current: Option<usize>, width: usize) -> (Vec<Row>, Option<Range<usize>>)
// Row { text: String, role: Past | Current | Future | Plain }
// the range is the rows of the current line
```

- **Wrapping** is by display width (the existing `ui::width` helper), at spaces where possible. A word wider than the pane is broken at the column. An empty line stays one empty row.
- **Synced scroll:** with no current line, the top is row 0. Otherwise the top is chosen so that the current line's first row sits at `inner_height / 2`, clamped to `0..=total - inner_height`.
- **Plain scroll:** with `span = total - inner_height`, `top = span * pos_ms / dur_ms`, computed in `u128` milliseconds (no float casts, so clippy pedantic stays quiet), then `usize::try_from(..).unwrap_or(span)` and `min(span)`. It is 0 when the lyrics fit, the duration is unknown or zero, or nothing is playing.
- **Styles:** `theme::PLAYING` for the current line, `theme::DIM` for past lines, and the default style for future and plain lines. No new theme role is needed.
- **Messages:** "Nothing playing" and "No lyrics" use the existing `render_empty` (italic `theme::MESSAGE`, top-left, the same look as an empty list; its `offset` argument is a throwaway local). While `!lyrics_ready()` the pane's inside is left empty (D4).
- Rows are rebuilt every frame. Lyrics are capped at 10 000 lines and most have under 100, so wrapping them at 30 frames a second is negligible. A cache keyed by width can come later if profiling shows a need.

### D7. Waking up for the next line

`App::lyrics_deadline(now) -> Option<Instant>` returns `now + (next_change(position) - position)` when the pane is shown, playback is `Playing`, and the lyrics are synced. The wait is at least `LYRICS_MIN_WAIT` (10 ms), so a position that has not quite reached the line's time can never cause a busy loop. `terminal::next_deadline` gains the extra argument and returns the earlier of the regular frame deadline and this one.

The highlight then moves within one input poll of the line's time. That is well inside the spec's 0.1 s, and when nothing is due there is no extra redraw. Plain lyrics need no deadline; their scroll moves by whole rows over minutes, and the 250 ms frame is enough.

- *Alternative: animate at 33 ms whenever synced lyrics are shown.* Rejected. It is about 8× the redraws while listening for a ≤ 33 ms gain in precision that the deadline already gives exactly.

### D8. Keys and docs

`keys::GROUPS` "Display" gains `l` — "show / hide the lyrics", `bar: &[]` — after `v`. "Display" stays second to last and every bar is unchanged. The README keys table gains the same row. `CLAUDE.md` is updated by each task group for the modules it adds or changes.

**Existing UI tests.** About 40 `ui.rs` tests draw at 100+ columns and hard-code the two-pane geometry (`MAIN_X`, title elision widths, track-table columns). Their fixtures (`library()`, `with_playlists()`, `many()`) call `set_lyrics_on(false)`, so they keep the two-pane layout unchanged. Pressing `l` in fixtures is avoided, because it can raise a notice that tests asserting the status line would see. Only the new lyrics tests turn the pane on.

## Risks / Trade-offs

- [Lyrics are read on the UI thread; a slow or stalled network mount can freeze the screen while a track starts] → The same mount already freezes the scan and the decoder open. The FIFO-safe open removes the pipe case, and reads are capped at 1 MiB. A background loader is a possible later change if this shows up in practice.
- [lofty's generic `Tag` may not expose ID3v2 `USLT` with a language or description as `UnsyncLyrics` in every case] → Task 2 writes real tagged files with lofty in tests (FLAC Vorbis comment, MP4, ID3v2 USLT). If a format does not round-trip, it is documented as unsupported rather than worked around.
- [Lyrics timing is offset by the audio device's buffer (tens of ms)] → Accepted, as for the visualizer. Users can fix files with `[offset:]`.
- [A directory listing per track change in huge flat folders] → It happens only when both direct probes miss, and it stops at 10 000 entries.
- [An empty or junk `.lrc` hides good tag lyrics] → A deliberate rule: a sidecar the user put there wins. Documented in the README.

## Migration Plan

None. This is a new pane, on by default. Terminals under 100 columns look exactly as before, and wider ones gain the pane, which `l` hides.
