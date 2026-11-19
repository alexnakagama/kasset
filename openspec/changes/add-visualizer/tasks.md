# Tasks

Every group ends green: `cargo test` (the whole suite) and `cargo clippy --all-targets -- -D warnings` pass before the group is handed off for review. Each group keeps the `//!` docs of the modules it touches, and its line in `CLAUDE.md`'s Architecture section, accurate.

## 1. Spectrum analysis (`Cargo.toml`, new `src/visualizer.rs`)

- [ ] 1.1 Add `rustfft = "6.4"` to `[dependencies]`. Run `cargo metadata --format-version 1` and check that no package in the tree has a `rust-version` above 1.89 (D8). Verify with `cargo +1.89 build` and a clean `cargo build`. If the check finds a higher MSRV, stop and raise it before continuing.
- [ ] 1.2 Add `src/visualizer.rs` with a `//!` doc stating its role and invariants (pure, no audio or terminal code, deterministic given `now`). Implement `Spectrum` as in D3:
  - `FFT_SIZE` = 2048, a Hann window, normalisation by the window sum;
  - log-spaced bands from 40 Hz to `min(16 kHz, rate / 2)`, peak magnitude per band, centre-bin rule for bands narrower than one bin;
  - mapping from −60 dB to 0 dB onto 0 to 1;
  - instant rise and a linear 1 s fall (`FALL_TIME`);
  - levels reset when the bar count changes;
  - `levels()` and `is_idle()`.

  Keep casts in small helpers with scoped `#[allow]`s and a comment on why each is in range. Register the module in `main.rs`. Verify with unit tests:
  - a 100 Hz sine peaks in the left third, and a 5 kHz sine in the right half, at 44.1, 48 and 96 kHz (spec *Low tone*, *High tone*);
  - a full-scale sine reaches near full height, and a sine at −70 dB gives zero;
  - `None`, silence and fewer than `FFT_SIZE` samples give zero targets;
  - after a full-height frame and then `None`, the level is about 0.5 at 0.5 s and 0 at 1 s, and `is_idle()` is true (spec *Sudden silence*);
  - band edges increase monotonically and the top edge is capped at Nyquist at 22.05 kHz;
  - a bar-count change resets the levels.

## 2. Audio tap (`src/player.rs`, `src/playback.rs`, `src/test_support.rs`)

- [ ] 2.1 Add `TapBuffer` (a ring of the newest `FFT_SIZE` mono samples plus the sample rate) as `tap: Mutex<TapBuffer>` in `SourceFlags`. In `Guarded`, mix each frame to mono and fill a 512-sample batch allocated in `Guarded::new`. Flush it with `try_lock`: drop the batch on `WouldBlock`, and recover from poison with `into_inner` (D1). Update the `player.rs` `//!` doc with the tap's invariants: it never blocks or allocates on the audio thread, and it is taken before the volume. Verify with unit tests driving `Guarded` over a rodio `SamplesBuffer`, with no device:
  - a stereo source with left = 1.0 and right = 0.0 fills the tap with 0.5;
  - the tap holds the newest `FFT_SIZE` samples in order, together with the source's sample rate;
  - with the tap lock held by the test, `next()` keeps returning samples and the batch is dropped.
- [ ] 2.2 Add `Audio::recent_samples(&self, out: &mut Vec<f32>) -> Option<u32>` with a default returning `None` (D2), and implement it for `Player` by copying the loaded track's tap. Add a `Playback::recent_samples` pass-through. Add `ToneAudio` to `src/test_support.rs`: it behaves like `SilentAudio` and returns a sine of configurable frequency and amplitude. Verify:
  - every existing test passes unchanged, since `SilentAudio` and `FakeAudio` are not edited;
  - a `Player`-level test (or a `Guarded` + `SourceFlags` test, if a device is unavailable) shows a new `load` starts with an empty tap (spec *Track change*);
  - `Playback::recent_samples` returns `ToneAudio`'s samples while playing.

## 3. App state and the `v` key (`src/app.rs`, `src/keys.rs`, `src/cli.rs`, `README.md`)

- [ ] 3.1 Add the `Visualizer` state to `App` (D4): `on` (initially `true`), `spectrum`, the fed-back `fits` and `bars`, and a reusable sample buffer.
  - Add `set_spectrum_layout(fits, bars)`, `visualizer_on()`, `spectrum_levels()` and `visualizer_animating()`.
  - In `tick`, update the spectrum only when `on && fits && bars > 0`, passing samples only while `PlayState::Playing`. When the pane is not shown, reset the levels instead.
  - Update the `app.rs` `//!` doc.

  Verify with tests using `ToneAudio` and an injected `Instant`:
  - while playing, the levels are non-zero;
  - after Space they fall to idle within about 1 s (spec *Paused*);
  - before any track is played they stay zero (spec *Nothing played yet*);
  - `visualizer_animating()` is true while playing and while falling, and false once idle, when off, or when `!fits`;
  - with `fits` false, no spectrum update happens.
- [ ] 3.2 Handle `v` in Normal mode from either pane:
  - it toggles `on`;
  - turning it on while `!fits` notifies "the visualizer needs a terminal at least 110 columns wide" as `Level::Info`.

  Verify with key-sequence tests:
  - `v` `v` toggles off and on (spec *Hide and show*, state level);
  - `v` with `fits` false gives the notice and leaves `on` true (spec *Turned on while too narrow*);
  - `/` then `v` adds `v` to the query and leaves `on` unchanged (spec *Not while typing*);
  - `v` in the help overlay and in a prompt keeps that mode's existing behaviour.
- [ ] 3.3 Add the "Display" group with the `v` entry ("show / hide the visualizer", `bar: &[]`) to `keys::GROUPS` (D7), and the same row to the README keys table. Verify:
  - the existing `keys.rs` tests pass, and every bar is unchanged;
  - a new test shows `help_text()` and `help_lines()` contain `v` and "show / hide the visualizer" (spec *Listed in the help*);
  - `cargo run -- --help` lists it.

## 4. Spectrum pane and frame rate (`src/ui.rs`, `src/theme.rs`, `src/terminal.rs`, `README.md`)

- [ ] 4.1 Add `theme::SPECTRUM` (magenta) with a doc comment, and update the `theme.rs` `//!` doc's list of roles. Verify that `cargo test` covers the theme invariants already there, and that no `Color` appears in `ui.rs` outside tests.
- [ ] 4.2 Implement the three-pane layout (D5): `THREE_PANE_MIN_WIDTH` = 110, `SPECTRUM_WIDTH` = (26, 40), and the pane with the unfocused border, title " Spectrum ", 1-column bars with 1-column gaps, `█` fill and `▁…▇` tops in `theme::SPECTRUM`. Add `spectrum_fits` / `spectrum_bars` to `ui::View`, and feed them back with `app.set_spectrum_layout` in `terminal::run`. Update the `ui.rs` `//!` doc. Verify with `TestBackend` tests:
  - at 120 columns, three panes left to right (spec *Three panes visible*, *Pane shown on a wide terminal*);
  - at 100 columns, two panes and `spectrum_fits` false (spec *Too narrow for the spectrum*);
  - at 60 columns, one pane;
  - after `v`, the pane is gone and the main pane is wider (spec *Hide and show*);
  - resizing from 120 to 90 and back hides and restores the pane while `on` stays true (spec *Narrowing the terminal*);
  - given levels of 0, 0.5 and 1 draw an empty column, a half-height bar with the correct partial glyph, and a full bar;
  - `spectrum_bars` equals half the inner width;
  - `Tab` twice never focuses the spectrum pane (spec *Tab skips the pane*);
  - every existing `ui.rs` test below 110 columns passes unchanged.
- [ ] 4.3 Add `ANIMATION_FRAME` (33 ms) and a pure `frame_interval(animating) -> Duration` to `terminal.rs`, and use `app.visualizer_animating()` in `event_loop` (D6). Update the `terminal.rs` `//!` doc. Verify with unit tests: `frame_interval(true)` is at most 40 ms, and `frame_interval(false)` equals `FRAME` (spec *Animating*, *Idle*).
- [ ] 4.4 Describe the spectrum pane in the README's description of the screen, next to the two-pane layout: when it appears, `v`, and what the bars show. Verify by reading it against the specs.

## 5. Integration checks

- [ ] 5.1 Run kasset in a real terminal at 120 columns or more. Play a track with clear bass and treble and check that:
  - the bars move smoothly;
  - Space makes them fall to zero within about a second;
  - `n` and seeking do not show the old track's spectrum;
  - lowering the volume does not shrink them;
  - narrowing the window below 110 columns hides the pane and widening restores it;
  - `v` hides and shows it.

  Check CPU use with `top`: while paused with the bars idle, it should be the same as 0.2.0; while playing, record the figure in the PR description.
- [ ] 5.2 Run the full checks from `CLAUDE.md`: `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo +1.89 build`. Confirm `openspec validate add-visualizer --strict` passes. Verify that all of them pass.

## Workflow follow-up

- Hand each group off to the kasset reviewer and fix its findings before the next group.
- After merge: sync the specs, archive the change, and release as 0.3.0 (PR with the version bump for the Release dry run, then tag `v0.3.0`).
