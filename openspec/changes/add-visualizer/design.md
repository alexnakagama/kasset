# Design

## Context

See `proposal.md` for the motivation, and `specs/audio-visualizer/spec.md` and `specs/player-tui/spec.md` for the required behaviour.

The parts of the current code that shape the approach:

- **Audio path.** `Player::load` (`src/player.rs`) wraps every decoder in `Guarded<S>`, a `rodio::Source` whose `next()` runs on rodio's audio thread and already sees every decoded sample (`rodio::Sample`, interleaved by channel). `rodio::Sample` is `f32` only because kasset does not enable rodio's `64bit` feature, so the code uses `rodio::Sample` and converts once. rodio's `Player::append` wraps the source as pausable, then amplify, and `set_volume` feeds only that amplify stage. A pausable source does not pull from its inner source while paused. Each load creates a fresh `Arc<SourceFlags>` shared between `Guarded` and `Loaded`. Seeking is a reload (`Playback::seek` calls `Audio::load` with the new start), so "a new load" covers seek, next/previous and replay.
- **Layering.** Everything above `player.rs` is generic over `A: Audio`; tests use `SilentAudio` and `FakeAudio`. `App` holds all UI state, and `ui.rs` renders it read-only. Sizes known only while drawing (`page_size`, `help_rows`) are fed back through `ui::View` by `terminal::run`.
- **Event loop.** `terminal::event_loop` ticks the app, draws, then waits up to `FRAME` (250 ms) for input.
- **Lints.** `unsafe_code = "forbid"`; clippy pedantic, which flags the `usize`/`f32` casts an FFT needs.
- **`Debug`.** `App` and `Playback` derive `Debug`, so every field type they hold must implement it.

## Goals / Non-Goals

**Goals:**
- Never block or allocate per sample on the audio thread.
- Keep the analysis a pure, deterministic module, testable with synthetic signals and an injected clock, like `queue.rs`.
- Cost nothing when the pane is not shown: no FFT, no fast redraw.

**Non-Goals:**
- Sample-accurate sync with the speakers. The device buffer delay (tens of ms) is accepted.
- Measuring what is actually sent to the device (after volume and mixing). The spec asks for the decoded audio.
- Reusing the tap for anything else, such as level meters or ReplayGain.

## Decisions

### D1. Tap inside `Guarded`, handed over with `try_lock`

`SourceFlags` gains a `tap: Mutex<TapBuffer>`. `TapBuffer` holds:
- a ring of the newest `FFT_SIZE` (2048) mono samples;
- the sample rate they were decoded at;
- `written: u64`, the total number of samples ever flushed into it.

`FFT_SIZE` is defined once, as `pub const` in `visualizer.rs`, and `player.rs` imports it. `TapBuffer` derives `Debug` and `Default`, so `SourceFlags` keeps both derives.

**Mixing to mono.** `Guarded` caches `channels` and `sample_rate`, which it already stores, and tracks the span itself. In rodio 0.23, `current_span_len()` returns the **total** size of the current span, not the samples left (rodio `source/mod.rs:181`). The Symphonia decoder reports its current packet buffer's length, and replaces that buffer only inside `next()` (`decoder/symphonia.rs:163`). So:
- `Guarded` keeps `span_left: usize`. It calls `inner.next()` first. If `span_left == 0` and no frame is half-built (`frame_count == 0`), it then reads `current_span_len()`, `channels()` and `sample_rate()`. It sets `span_left` to the reported length, and counts the sample just read in the new format. Reading after `next()` sees the new buffer, not the old one.
- `span_left` goes down by one per sample. A reported length of `None` or `Some(0)` means "unknown": `span_left` is set to 4096 samples, so the values are not re-read on every sample.
- Re-reads happen only on a frame boundary. A span ends on one by rodio's contract, and the 4096 fallback is not a multiple of 3 or 6 channels, so it could otherwise split a frame.

The per-sample path stays an add and two counters.
- Each sample (`rodio::Sample`) is added to a running frame sum.
- When a frame has `channels` samples, their mean is pushed as `f32` onto a local batch: a `Vec<f32>` with capacity 512, allocated once in `Guarded::new`.
- If the channel count changes, the partial frame is discarded.
- If the sample rate changes, the pending batch is discarded, and the next flush clears the ring before appending. Samples at two different rates are never mixed in one window.

**Handing over.** When the batch is full, `Guarded` calls `tap.try_lock()`:
- **On success:** it appends the batch to the ring (dropping the oldest samples), records the sample rate, adds the batch length to `written`, and clears the batch.
- **On `WouldBlock`:** it clears the batch and the samples are lost.
- **On a poisoned lock:** it recovers with `into_inner` as elsewhere.

The audio thread therefore never waits for the UI. A partial batch at the end of a track is never flushed; at 44.1 kHz that is less than 12 ms of audio.

A fresh `SourceFlags`, and with it a fresh tap, comes with every `load`. Seeking or changing track therefore drops old audio without any explicit clearing (spec: *Bars follow playback state*).

The tap is taken **before** the volume: rodio applies the volume in an amplify stage downstream of `Guarded`. The bars therefore do not depend on the volume (spec: *Spectrum bands*).

- *Alternative: a lock-free ring buffer crate (`ringbuf`).* Rejected. It is one more dependency for a buffer whose losses are harmless, and `std::sync::Mutex::try_lock` already never blocks.
- *Alternative: rodio's `periodic_access`.* Rejected. It gives periodic access to the source, not to the samples flowing through it.
- *Alternative: tap after the mixer, in the device callback.* Rejected. rodio does not expose the callback's output, and the result would scale with the volume.

### D2. `Audio::recent_samples`

The `Audio` trait gains:

```rust
/// What `recent_samples` found.
pub struct Recent {
    /// Sample rate of the samples copied, in Hz.
    pub sample_rate: u32,
    /// Total samples ever decoded into the tap; unchanged between two calls
    /// means no new audio arrived.
    pub written: u64,
}

/// Clears `out`, then appends the newest decoded mono samples (at most
/// `visualizer::FFT_SIZE`, oldest first). Returns `None` when no track is
/// loaded or nothing has been decoded yet.
fn recent_samples(&self, out: &mut Vec<f32>) -> Option<Recent> { None }
```

`Player` locks the loaded track's tap (on the UI thread, so a brief wait is fine) and copies the ring in order. It converts rodio's `NonZero<u32>` sample rate with `.get()`. The default body means `SilentAudio`, `FakeAudio` and every existing test compile unchanged. A new test fake, `ToneAudio` in `test_support.rs`, returns a generated sine wave for App and UI tests.

`Playback` exposes `recent_samples` as a pass-through, so `App` never reaches past `Playback` to the audio.

### D3. Analysis in a new pure module, `src/visualizer.rs`

`Spectrum` owns:
- the FFT, planned once with `FftPlanner::<f32>::new().plan_fft_forward(FFT_SIZE)`, which returns an `Arc<dyn Fft<f32>>`;
- a precomputed Hann window;
- reusable scratch buffers;
- `levels: Vec<f32>` (0.0 to 1.0, one per bar);
- the `Instant` of the last update.

It has one entry point:

```rust
pub fn update(&mut self, input: Option<(&[f32], u32)>, bars: usize, now: Instant)
```

1. **Targets.** If `input` is `None`, or holds fewer than `FFT_SIZE` samples, every target is 0. Otherwise:
   1. Window the samples and run the FFT.
   2. Compute each bin's magnitude as `2 · |X[k]| / Σw`. A real sine of amplitude A peaks at `A · Σw / 2` in the one-sided spectrum, so this makes a full-scale sine read about 0 dB. Dividing by `Σw` alone would read it at −6 dB. Between bins, the Hann window can lose up to about 1.4 dB more (scalloping).
   3. Split 40 Hz to `min(16 kHz, rate / 2)` into `bars` logarithmically spaced bands. Each band's value is the peak magnitude of the bins it covers. A band narrower than one bin uses the bin containing its centre frequency: at 44.1 kHz with 2048 points, bins are about 21.5 Hz wide, so the lowest bands share bins.
   4. Map each band from −60 dB to 0 dB onto 0 to 1, clamped.
2. **Motion.** `level = max(target, level - dt / FALL_TIME)`, with `FALL_TIME` = 1 s and `dt` taken from `now`. Bars rise instantly and fall linearly (spec: *Bar motion*). A large `dt` after a pause just takes the bars to zero.
3. **Resize.** When `bars` differs from `levels.len()`, the levels are reset to zero. Bars rise instantly, so the next frame is already right.

`is_idle()` reports whether every level is 0. All casts live in small helpers with a targeted `#[allow(clippy::cast_…)]` and a comment on why each is in range.

`rustfft::Fft` does not implement `Debug`, and `App` derives it. `Spectrum` therefore implements `fmt::Debug` by hand: it shows the levels and ends with `finish_non_exhaustive()`.

- *Alternative: average power per band instead of peak.* Rejected. A band spanning many treble bins averages a lone tone down to nothing. Peak keeps a single tone visible, which the *High tone* scenario relies on.
- *Alternative: write our own FFT.* Rejected during exploration. `rustfft` is the standard, well-tested crate; see D8 for the dependency check.

### D4. App state and the `v` key

`App` gains a `visualizer: Visualizer` field, holding:
- `on: bool` (initially `true`);
- `spectrum: Spectrum`;
- `bars: usize` and `fits: bool`, fed back from the last frame (D5);
- a reusable sample buffer.

`App::tick(now)` updates the spectrum only when `on && fits && bars > 0`. It passes the samples only while `playback.state()` is `Playing` and new audio is arriving, and `None` otherwise. Paused, stopped, idle and stalled tracks therefore all fall to zero (spec: *Bars follow playback state*).

`Visualizer` keeps the last `written` value and the `Instant` it last changed. If `written` has not advanced for `STALE_AFTER` (250 ms), the tap is stale and `None` is passed. `Guarded::next` runs inside the device callback, so `written` advances in bursts of one callback's worth of audio. 250 ms (the idle frame) leaves room for large ALSA, CoreAudio or WASAPI buffers, and the 1 s fall makes the extra delay harmless. It is a single constant, tuned in the 5.1 check if bars flicker. This covers a decoder that has ended before `Playback::tick` notices, and an output device that stopped pulling. Without it, the bars would freeze on the last window instead of falling. When the pane is not shown, the spectrum is not updated at all: no FFT runs. Its levels are reset, so the bars do not show a stale frame when the pane comes back.

`v` in Normal mode, from either pane, flips `on`. While `!fits`, the change cannot be seen, so `v` also calls `notify` with an `Info` notice:
- turning it on: "visualizer on: needs a terminal at least 110 columns wide";
- turning it off: "visualizer off".

When the pane is visible, the change speaks for itself and no notice is shown. In Search and in the prompts, `v` keeps its current meaning (typed text, or "any other key"), because those modes handle keys before Normal-mode dispatch.

New read-only accessors for `ui.rs` and `terminal.rs`:
- `visualizer_on()`;
- `spectrum_levels() -> &[f32]`;
- `visualizer_animating()`: `on && fits && (state == Playing || !spectrum.is_idle())`.

### D5. Layout: three panes from 110 columns

`ui.rs` gains:
- `THREE_PANE_MIN_WIDTH: u16 = 110`;
- `SPECTRUM_WIDTH: (u16, u16) = (26, 40)`, sized like the sidebar: `body_width / 4`, clamped.

At 110 columns the layout is sidebar 27, main 56, spectrum 27; the main pane is never narrower than in a 70-column two-pane layout.

`render_body` picks the layout:
- `width >= 110 && app.visualizer_on()`: three panes;
- `width >= 70`: two panes, as today;
- otherwise one pane, as today.

The spectrum pane:
- always uses the unfocused border style, since it never has focus, with the title " Spectrum ";
- draws bars 1 column wide with a 1-column gap between them, so `bars = (inner_width + 1) / 2`. These use `2 · bars − 1` columns, which fills an odd width exactly and leaves one empty column at the right of an even one;
- draws `min(levels.len(), bars)` bars, so the first frame after a resize, drawn before the new bar count reaches `App`, cannot index out of range. On the next tick the levels are reset to the new count, and they rise again at once;
- fills each bar from the bottom: full rows are `█`, and the top cell is one of `▁▂▃▄▅▆▇` from `level * rows * 8` eighths.

`ui::View` gains `spectrum_fits: bool` (`body width >= 110`, whether or not the pane is on) and `spectrum_bars: usize` (0 when not drawn). `terminal::event_loop` feeds both back with `app.set_spectrum_layout(fits, bars)`, next to `set_page_size`. `fits` is recorded even while the pane is off, so `v` can tell whether the pane would appear.

### D6. Frame interval follows the visualizer

`terminal.rs` gains `ANIMATION_FRAME = Duration::from_millis(33)`, about 30 fps.

The loop keeps a deadline, `next_frame`: the time the current frame started plus `ANIMATION_FRAME` while `app.visualizer_animating()`, or plus `FRAME` otherwise. It polls for input until that deadline (zero if it has passed). Polling for a fixed 33 ms after drawing would add the draw time to every frame, leaving no margin over the spec's 25 fps on a slow machine.

- **Idle cost.** When paused, the bars fall for up to a second at 30 fps, then the loop drops back to 250 ms. A hidden pane never speeds it up (spec: *Visualizer refresh rate*).
- **Other per-tick work.** `App::tick` also runs `playback.tick()` and the status expiry every frame. Both are cheap: `playback.tick()` reads `audio.status()` and makes at most a bounded number of retries per call. Retries and failure counting are per tick, so a run of unplayable tracks is skipped about 7 times faster while animating. Animation only runs while a track is playing, so this rarely matters, and the skip limit still holds.
- **Redraw cost.** ratatui only sends changed cells, so 30 fps over SSH is a few hundred bytes per frame.

The deadline is computed by a pure function, `next_deadline(frame_start: Instant, animating: bool) -> Instant`, which is unit-tested.

### D7. Theme and keys

`theme.rs` gains `SPECTRUM: Style::new().fg(Color::Magenta)`. `theme.rs` uses Green, Cyan, Yellow, Red and DarkGray, so Magenta (like Blue) is still free, and each colour keeps one meaning. The bars' height carries the information, so colour is never the only cue.

`keys.rs` gains a group titled "Display" with one entry: `v`, "show / hide the visualizer", with `bar: &[]`. It is inserted **before** "Help & quit", after "Navigation & search". "Help & quit" must stay last, because a `ui.rs` help-overlay test (`src/ui.rs:1854`) checks that the last line after `End` is `q, Ctrl+C`. The key bar is already full in Normal mode, and `v` is not essential. This feeds `kasset --help` and the `?` overlay. The README keys table gets the same row.

### D8. Dependency

`rustfft = "6.4"`. Its own MSRV is 1.61. Its dependencies (`num-complex`, `num-traits`, `num-integer`, `strength_reduce`, `transpose`, `primal-check`) must be checked with `cargo metadata` after adding it, as `CLAUDE.md` requires. `rustfft` uses `unsafe` internally for SIMD, which does not conflict with `unsafe_code = "forbid"`: that lint applies only to kasset's own crate.

### D9. Testing

- **`visualizer.rs`** (pure, with an injected `Instant`):
  - a 100 Hz sine peaks in the left third, and a 5 kHz sine in the right half, at 44.1, 48 and 96 kHz;
  - silence, `None` and too few samples give zero targets;
  - a full-scale sine centred on a bin reads at least 0.95 (−3 dB), allowing for Hann scalloping;
  - the fall takes about `FALL_TIME`;
  - band edges increase monotonically, and the top band is capped at Nyquist for low sample rates;
  - a bar-count change resets the levels.
- **`player.rs`:**
  - `Guarded` over a rodio `SamplesBuffer` (no device needed) mixes stereo to mono and fills the tap;
  - a held lock makes `Guarded` drop the batch without blocking;
  - each `load` gets a new tap;
  - the tap holds exactly the decoded samples (mixed to mono), whatever the sink volume, which covers spec *Volume does not matter*;
  - a two-span source whose channel count changes mid-stream mixes each span correctly, and a sample-rate change clears the ring;
  - `written` grows by each flushed batch.
- **`app.rs`:** key sequences with `ToneAudio`:
  - `v` toggles;
  - `v` while `!fits` gives the "on" or "off" notice, and gives none while the pane is visible;
  - `v` during search is typed;
  - pause makes the levels fall to idle;
  - a `ToneAudio` that stops advancing `written` while still `Playing` makes the levels fall after `STALE_AFTER`;
  - `visualizer_animating` in each state.
- **`ui.rs`** (`TestBackend`):
  - three panes at 120 columns, two at 100, one at 60;
  - pane hidden after `v`;
  - given levels draw the expected glyph columns;
  - drawing at 120 columns and then at 140 without a tick neither panics nor draws stray glyphs;
  - `View` reports `spectrum_fits` and `spectrum_bars`.
- **`keys.rs` / `cli.rs`:** `v` appears in `help_text` and `help_lines`, and "Help & quit" is still the last group.
- **`terminal.rs`:** `next_deadline` is 33 ms after the frame start while animating and 250 ms otherwise.
- **Existing tests:** every one must keep passing unchanged. Layouts below 110 columns are untouched.

## Risks / Trade-offs

- [Extra work on the audio thread for every sample] → It is one add, a counter and an occasional vector push, plus one `try_lock` per 512 frames. That is negligible next to decoding. No allocation happens after `Guarded::new`.
- [The tap loses batches while the UI holds the lock] → The UI holds the lock only to copy 2048 floats. A lost batch only means one slightly older spectrum frame.
- [The 2048-point window adds about 46 ms of history at 44.1 kHz] → Bars react a little slower than a smaller window would, in exchange for usable bass resolution. Accepted.
- [Low bands share FFT bins and move together] → Inherent at this window size. The centre-bin rule keeps them stable instead of flickering.
- [The 30 fps redraw costs CPU while playing] → It only happens with the pane visible and audio playing; `v` turns it off. On slow links, ratatui's diff keeps the output small.
- [More compile time and binary size from `rustfft` and its dependencies] → The crates are small and have no C code. Release builds on all 5 targets are re-checked by the PR dry run, since `Cargo.toml` changes.
- [Clippy pedantic cast lints in numeric code] → Casts are kept in a few documented helpers with narrowly scoped `allow`s, not a module-wide allow.

## Migration Plan

Nothing to migrate: no stored data or settings change, and no key changes meaning. Rollback is a plain revert. Release as a minor version (0.3.0) after merge, following the README release steps.

## Open Questions

- The exact spectrum width clamp `(26, 40)` and the colour (magenta) can be tuned after trying it in a real terminal, without spec changes.
