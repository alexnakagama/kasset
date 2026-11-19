# Proposal

## Why

kasset 0.2.0 has a two-pane layout, but nothing on screen shows the music itself. A spectrum visualizer in a strip just above the now-playing area gives kasset the feel of a modern TUI player, next to the track that is playing, without changing anything on short terminals.

## What Changes

- **Spectrum strip.** On terminals at least 24 rows tall, a "Spectrum" strip, 6 rows tall including its border, spans the full width between the panes and the now-playing area and shows frequency bars for the audio that is playing. It appears at every width, in the two-pane and the one-pane layout. The sidebar and the main pane keep their behaviour and their width rules (two panes, or one below 70 columns); they only give up the strip's rows. Below 24 rows the screen is exactly as in 0.2.0.
- **Spectrum bars.**
  - Bands are spaced logarithmically from about 40 Hz to 16 kHz, so bass, mids and treble get a fair share of bars.
  - Bar heights use a decibel scale and are drawn with `▁▂▃▄▅▆▇█` partial blocks.
  - Bars rise at once and fall back smoothly.
  - They fall to zero when playback is paused, stopped or between tracks.
  - Volume changes do not shrink them: the visualizer shows the music, not the output level.
- **Smooth refresh.** While the strip is shown and a track is playing, the screen redraws about 30 times a second instead of 4. In every other state it keeps today's 250 ms refresh, so an idle kasset uses no more CPU than 0.2.0.
- **`v` key.** The strip is on by default; `v` hides or shows it. On a terminal too short for the strip, where the toggle cannot be seen, `v` shows a status message saying the visualizer is now off, or on but in need of a taller terminal. The strip appears once the terminal is tall enough. The setting lasts until kasset quits. `v` is listed in `kasset --help`, the `?` overlay and the README keys table.
- The strip never takes keyboard focus: `Tab` still moves only between the sidebar and the main pane.
- No **BREAKING** changes: no existing key changes meaning, and terminals shorter than 24 rows look the same.

## Capabilities

### New Capabilities
- `audio-visualizer`: The spectrum display of the audio being played. It covers what the bars represent (frequency range, scale, rise and fall), how they react to pause, stop, seek and track changes, independence from the volume, and the refresh rate while visible.

### Modified Capabilities
- `player-tui`:
  - Screen layout: a spectrum strip between the panes and the now-playing area on tall terminals. The width rules (Narrow terminals) are unchanged.

## Impact

- `src/player.rs`:
  - The `Guarded` source wrapper copies decoded samples, mixed down to mono, into a small shared buffer. It never blocks the audio thread: a batch is dropped if the buffer is busy.
  - `Audio` gains a method that returns the latest samples and the sample rate. Its default returns nothing, so the test fakes need no change.
  - A new buffer is created with every `load`, so seeking or changing track clears it.
- New `src/visualizer.rs`: the analysis, with no audio or terminal code: windowing, FFT, log-spaced bands, decibel scaling, rise and fall smoothing. It is tested with synthetic tones.
- `src/app.rs`: the visualizer state, updated in `App::tick`; `v` handling; the "needs a taller terminal" status message.
- `src/ui.rs`: the strip layout and drawing the bars. The number of bars and whether the strip fits are fed back through `ui::View`, like `page_size` already is.
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
  - a visualizer on terminals shorter than 24 rows;
  - a spectrum pane beside the track lists (a third column) instead of the strip.
