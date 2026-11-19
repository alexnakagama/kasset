# Proposal

## Why

kasset 0.2.0 has a two-pane layout, but on a wide terminal the track list stretches across the whole width and nothing on screen shows the music itself. A spectrum visualizer in a third pane on the right gives kasset the feel of a modern TUI player and uses the space that wide terminals leave idle, without changing anything on normal-width terminals.

## What Changes

- **Third pane.** On terminals at least 110 columns wide, a "Spectrum" pane on the right shows frequency bars for the audio that is playing. The sidebar and the main pane keep their behaviour. Below 110 columns the layout is exactly as in 0.2.0 (two panes, or one below 70).
- **Spectrum bars.**
  - Bands are spaced logarithmically from about 40 Hz to 16 kHz, so bass, mids and treble get a fair share of bars.
  - Bar heights use a decibel scale and are drawn with `▁▂▃▄▅▆▇█` partial blocks.
  - Bars rise at once and fall back smoothly.
  - They fall to zero when playback is paused, stopped or between tracks.
  - Volume changes do not shrink them: the visualizer shows the music, not the output level.
- **Smooth refresh.** While the pane is shown and a track is playing, the screen redraws about 30 times a second instead of 4. In every other state it keeps today's 250 ms refresh, so an idle kasset uses no more CPU than 0.2.0.
- **`v` key.** The pane is on by default; `v` hides or shows it. On a terminal too narrow for the pane, where the toggle cannot be seen, `v` shows a status message saying the visualizer is now off, or on but in need of a wider terminal. The pane appears once the terminal is wide enough. The setting lasts until kasset quits. `v` is listed in `kasset --help`, the `?` overlay and the README keys table.
- The pane never takes keyboard focus: `Tab` still moves only between the sidebar and the main pane.
- No **BREAKING** changes: no existing key changes meaning, and narrow terminals look the same.

## Capabilities

### New Capabilities
- `audio-visualizer`: The spectrum display of the audio being played. It covers what the bars represent (frequency range, scale, rise and fall), how they react to pause, stop, seek and track changes, independence from the volume, and the refresh rate while visible.

### Modified Capabilities
- `player-tui`:
  - Screen layout: a third pane on wide terminals.
  - Narrow terminals: the thresholds for three, two and one panes.

## Impact

- `src/player.rs`:
  - The `Guarded` source wrapper copies decoded samples, mixed down to mono, into a small shared buffer. It never blocks the audio thread: a batch is dropped if the buffer is busy.
  - `Audio` gains a method that returns the latest samples and the sample rate. Its default returns nothing, so the test fakes need no change.
  - A new buffer is created with every `load`, so seeking or changing track clears it.
- New `src/visualizer.rs`: the analysis, with no audio or terminal code: windowing, FFT, log-spaced bands, decibel scaling, rise and fall smoothing. It is tested with synthetic tones.
- `src/app.rs`: the visualizer state, updated in `App::tick`; `v` handling; the "needs a wider terminal" status message.
- `src/ui.rs`: the three-pane layout and drawing the bars. The number of bars and whether the pane fits are fed back through `ui::View`, like `page_size` already is.
- `src/terminal.rs`: the poll interval depends on whether the visualizer is animating.
- `src/theme.rs`: one new role for the bars.
- `src/keys.rs`, `README.md`: the `v` entry.
- **New dependency:** `rustfft` 6.x (MSRV 1.61; kasset's `rust-version` stays 1.89). Re-check `rust-version` against `cargo metadata` after adding it.
- Specs: new `audio-visualizer`; `player-tui` modified. The `audio-playback` behaviour is unchanged.
- **Out of scope:**
  - cover art;
  - other visualizer styles (waveform, level meter);
  - user-configurable colours, band counts or frequency range;
  - remembering the `v` setting between runs;
  - a visualizer on terminals narrower than 110 columns.
