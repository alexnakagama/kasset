# Tasks

Every group ends green: `cargo test` (the whole suite) and `cargo clippy --all-targets -- -D warnings` pass before the group is handed off for review. Each group keeps the `//!` docs of the modules it touches, and its line in `CLAUDE.md`'s Architecture section, accurate.

## 1. Spectrum analysis (`Cargo.toml`, new `src/visualizer.rs`)

- [x] 1.1 Add `rustfft = "6.4"` to `[dependencies]`. Run `cargo metadata --format-version 1` and check that no package in the tree has a `rust-version` above 1.89 (D8). Verify with `cargo +1.89 build` and a clean `cargo build`. If the check finds a higher MSRV, stop and raise it before continuing.
- [x] 1.2 Add `src/visualizer.rs` with a `//!` doc stating its role and invariants (pure, no audio or terminal code, deterministic given `now`). Implement `Spectrum` as in D3:
  - `pub const FFT_SIZE` = 2048 (also used by `player.rs`), a Hann window, and FFT planning with `FftPlanner::<f32>::new().plan_fft_forward`;
  - bin magnitudes as `2 · |X[k]| / Σw`, so a full-scale sine reads about 0 dB;
  - log-spaced bands from 40 Hz to `min(16 kHz, rate / 2)`, peak magnitude per band, centre-bin rule for bands narrower than one bin;
  - mapping from −60 dB to 0 dB onto 0 to 1;
  - instant rise and a linear 1 s fall (`FALL_TIME`);
  - levels reset when the bar count changes;
  - `levels()` and `is_idle()`;
  - a manual `impl fmt::Debug for Spectrum` that shows the levels, because `rustfft::Fft` has no `Debug` and `App` derives it.

  Keep casts in small helpers with scoped `#[allow]`s and a comment on why each is in range. Register the module in `main.rs`. Verify with unit tests:
  - a 100 Hz sine peaks in the left third, and a 5 kHz sine in the right half, at 44.1, 48 and 96 kHz (spec *Low tone*, *High tone*);
  - a full-scale sine centred on a bin reaches at least 0.95, and a sine at −70 dB gives zero;
  - `format!("{spectrum:?}")` works;
  - `None`, silence and fewer than `FFT_SIZE` samples give zero targets;
  - after a full-height frame and then `None`, the level is about 0.5 at 0.5 s and 0 at 1 s, and `is_idle()` is true (spec *Sudden silence*);
  - band edges increase monotonically and the top edge is capped at Nyquist at 22.05 kHz;
  - a bar-count change resets the levels.

## 2. Audio tap (`src/player.rs`, `src/playback.rs`, `src/test_support.rs`)

- [x] 2.1 Add `TapBuffer` (a ring of the newest `FFT_SIZE` mono samples, the sample rate, and `written: u64`, deriving `Debug`, with a manual `Default` that preallocates the ring so the audio thread never allocates) as `tap: Mutex<TapBuffer>` in `SourceFlags` (D1). In `Guarded`:
  - mix each frame of `rodio::Sample`s to mono `f32` and fill a 512-sample batch allocated in `Guarded::new`;
  - track the span with `span_left`. After `inner.next()`, when `span_left == 0` and no frame is half-built, read `current_span_len()`, `channels()` and `sample_rate()`. That function returns the total span size, not the remainder. Treat `None` or `Some(0)` as 4096 samples. On a rate change, discard the batch and clear the ring (D1). Because the format is re-read only on a frame boundary, a channel-count change never meets a half-built frame;
  - flush the batch with `try_lock`: drop it on `WouldBlock`, recover from poison with `into_inner`, and add the flushed length to `written`.

   Update the `player.rs` `//!` doc with the tap's invariants: it never blocks or allocates on the audio thread, and it is taken before the volume. Verify with unit tests driving `Guarded` over a rodio `SamplesBuffer`, with no device:
  - a stereo source with left = 1.0 and right = 0.0 fills the tap with 0.5;
  - the tap holds the newest `FFT_SIZE` samples in order, together with the source's sample rate;
  - with the tap lock held by the test, `next()` keeps returning samples and the batch is dropped;
  - the tap equals the decoded samples whatever the volume, since the tap is upstream of rodio's amplify stage (spec *Volume does not matter*);
  - a two-span source whose channel count changes is mixed correctly per span, and a rate change clears the ring;
  - a span whose length is not a multiple of the old frame size, and a source reporting `current_span_len() == Some(0)` or `None`, keep frames aligned without re-reading on every sample;
  - `written` grows by each flushed batch.
- [x] 2.2 Add `Recent { sample_rate: u32, written: u64 }` and `Audio::recent_samples(&self, out: &mut Vec<f32>) -> Option<Recent>` with a default returning `None` (D2). It clears `out` and appends the whole ring. Implement it for `Player` by copying the loaded track's tap, converting the rate with `.get()`. Add a `Playback::recent_samples` pass-through. Add `ToneAudio` to `src/test_support.rs`: it behaves like `SilentAudio`, returns a sine of configurable frequency and amplitude, and can be told to stop advancing `written`. Verify:
  - every existing test passes unchanged, since `SilentAudio` and `FakeAudio` are not edited;
  - a `Player`-level test (or a `Guarded` + `SourceFlags` test, if a device is unavailable) shows a new `load` starts with an empty tap (spec *Track change*);
  - `Playback::recent_samples` returns `ToneAudio`'s samples while playing.

## 3. App state and the `v` key (`src/app.rs`, `src/keys.rs`, `src/cli.rs`, `README.md`)

- [x] 3.1 Add the `Visualizer` state to `App` (D4): `on` (initially `true`), `spectrum`, the fed-back `fits` and `bars`, and a reusable sample buffer.
  - Add `set_spectrum_layout(fits, bars)`, `visualizer_on()`, `spectrum_levels()` and `visualizer_animating()`.
  - In `tick`, update the spectrum only when `on && fits && bars > 0`. Pass samples only while `PlayState::Playing` and `written` has advanced within `STALE_AFTER` (250 ms); otherwise pass `None`. When the strip is not shown, reset the levels instead.
  - Update the `app.rs` `//!` doc.

  Verify with tests using `ToneAudio` and an injected `Instant`:
  - while playing, the levels are non-zero;
  - after Space they fall to idle within about 1 s (spec *Paused*);
  - before any track is played they stay zero (spec *Nothing played yet*);
  - `visualizer_animating()` is true while playing and while falling, and false once idle, when off, or when `!fits`;
  - with `fits` false, no spectrum update happens;
  - when `ToneAudio` stops advancing `written` while `Playing`, the levels fall to idle after `STALE_AFTER`.
- [x] 3.2 Handle `v` in Normal mode from either pane:
  - it toggles `on`;
  - while `!fits`, it notifies as `Level::Info`: "Visualizer on: needs a terminal at least 24 rows tall" when turning it on, "Visualizer off" when turning it off;
  - while the strip is visible, it shows no notice.

  Verify with key-sequence tests:
  - `v` `v` toggles off and on (spec *Hide and show*, state level);
  - from off, `v` with `fits` false gives the "on" notice and leaves `on` true (spec *Turned on while too short*);
  - from on, `v` with `fits` false gives the "off" notice (spec *Turned off while too short*);
  - `v` with `fits` true gives no notice;
  - `/` then `v` adds `v` to the query and leaves `on` unchanged (spec *Not while typing*);
  - `v` in the help overlay and in a prompt keeps that mode's existing behaviour.
- [x] 3.3 Add the "Display" group with the `v` entry ("show / hide the visualizer", `bar: &[]`) to `keys::GROUPS`, inserted after "Navigation & search" and before "Help & quit" (D7). Add the same row to the README keys table. Verify:
  - the existing `keys.rs` tests and the `ui.rs` help-overlay tests pass unchanged; the one at `src/ui.rs:1854` needs "Help & quit" to stay last;
  - every bar is unchanged;
  - a new test shows `help_text()` and `help_lines()` contain `v` and "show / hide the visualizer" (spec *Listed in the help*);
  - `cargo run -- --help` lists it.

## 4. Spectrum strip and frame rate (`src/ui.rs`, `src/theme.rs`, `src/terminal.rs`, `README.md`)

- [x] 4.1 Add `theme::SPECTRUM` (magenta) with a doc comment, and update the `theme.rs` `//!` doc's list of roles. Verify that `cargo test` covers the theme invariants already there, and that no `Color` appears in `ui.rs` outside tests.
- [x] 4.2 Implement the spectrum strip (D5): `SPECTRUM_HEIGHT` = 6 and `SPECTRUM_MIN_HEIGHT` = 24. Split the body into `[panes, strip]` when the visualizer is on and the terminal is at least 24 rows tall; draw the panes into `panes` with the existing width rules; keep popups centred over the whole body. The strip spans the full width, with the unfocused border, title " Spectrum ", 1-column bars with 1-column gaps (`bars = (inner_width + 1) / 2`, drawing `min(levels.len(), bars)`), `█` fill and `▁…▇` tops in `theme::SPECTRUM`. Add `spectrum_fits` / `spectrum_bars` to `ui::View`, and feed them back with `app.set_spectrum_layout` in `terminal::event_loop`, next to `set_page_size`. Update the `ui.rs` `//!` doc and its line in `CLAUDE.md`. Verify with `TestBackend` tests:
  - at 80×30, two panes with the strip below them spanning the full width, above the now-playing area (spec *Strip shown on a tall terminal*, *Spectrum strip visible*);
  - at 80×20, no strip and `spectrum_fits` false (spec *Too short for the strip*);
  - at 60×30, one pane with the strip below it (spec *Shown in the one-pane layout*);
  - after `v`, the strip is gone and the panes are taller (spec *Hide and show*);
  - resizing from 30 to 20 rows and back hides and restores the strip while `on` stays true (spec *Shortening the terminal*);
  - given levels of 0, 0.5 and 1 draw an empty column, a half-height bar with the correct partial glyph, and a full bar;
  - `spectrum_bars` equals `(inner_width + 1) / 2`;
  - drawing at 100 columns and then at 140 without a tick neither panics nor draws stray glyphs;
  - a popup (the picker) is centred over the panes and the strip together;
  - `Tab` twice never focuses the spectrum strip (spec *Tab skips the strip*);
  - every existing `ui.rs` test passes unchanged, including those drawn at 24 rows or more (picker 60×24, help overlay 100×50, 120×50, 40×60, 80×30). If one depends on the panes' height, stop and raise it.
- [x] 4.3 Add `ANIMATION_FRAME` (33 ms) and a pure `next_deadline(frame_start, animating) -> Instant` to `terminal.rs`. In `event_loop`, poll for input until that deadline instead of a fixed `FRAME`, using `app.visualizer_animating()` (D6). Update the `terminal.rs` `//!` doc. Verify with unit tests: the deadline is 33 ms after the frame start while animating and `FRAME` after it otherwise, and a deadline already passed polls with zero (spec *Animating*, *Idle*).
- [x] 4.4 Describe the spectrum strip in the README's description of the screen, next to the two-pane layout: when it appears, `v`, and what the bars show. Verify by reading it against the specs.

## 5. Integration checks

- [x] 5.1 Run kasset in a real terminal at 120 columns or more. Play a track with clear bass and treble and check that:
  - the bars move smoothly;
  - Space makes them fall to zero within about a second;
  - `n` and seeking do not show the old track's spectrum;
  - lowering the volume does not shrink them;
  - shortening the window below 24 rows hides the strip and making it taller restores it;
  - `v` hides and shows it;
  - the bars do not flicker while playing, which would mean `STALE_AFTER` is too short for this device's buffer.

  Check CPU use with `top`: while paused with the bars idle, it should be the same as 0.2.0; while playing, record the figure in the PR description.
- [x] 5.2 Run the full checks from `CLAUDE.md`: `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo +1.89 build`. Confirm `openspec validate add-visualizer --strict` passes. Verify that all of them pass.

## Workflow follow-up

- Hand each group off to the kasset reviewer and fix its findings before the next group.
- After merge: sync the specs, archive the change, and release as 0.3.0 (PR with the version bump for the Release dry run, then tag `v0.3.0`).
