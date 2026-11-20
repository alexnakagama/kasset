# audio-visualizer Specification

## Purpose

Shows a live frequency spectrum of the audio kasset is playing, as bars in a strip between the track lists and the now-playing area, so the screen reflects the music itself.

## Requirements

### Requirement: Spectrum strip
When the visualizer is on and the terminal is tall enough, the system SHALL show a strip titled "Spectrum", 6 rows tall including its border, across the full width between the panes and the now-playing area. It SHALL draw one vertical bar per frequency band, lowest band on the left, filling the strip's inner height at full level. The strip SHALL never take keyboard focus.

#### Scenario: Strip shown on a tall terminal
- **WHEN** the terminal is 80 columns wide and 30 rows tall and kasset starts
- **THEN** a "Spectrum" strip is shown across the full width, below the panes and above the now-playing area

#### Scenario: Shown in the one-pane layout
- **WHEN** the terminal is 60 columns wide and 30 rows tall
- **THEN** only the focused pane is shown, with the spectrum strip below it

#### Scenario: Tab skips the strip
- **WHEN** the spectrum strip is shown, the main pane has focus, and the user presses `Tab` twice
- **THEN** focus moves to the sidebar and back to the main pane, never to the spectrum strip

### Requirement: Strip needs a tall terminal
The system SHALL show the spectrum strip only on terminals at least 24 rows tall, at every width. On a shorter terminal the screen SHALL look as it does without the visualizer.

#### Scenario: Too short for the strip
- **WHEN** the terminal is 20 rows tall and the visualizer is on
- **THEN** no spectrum strip is shown

#### Scenario: Shortening the terminal
- **WHEN** the spectrum strip is shown and the terminal is resized from 30 to 20 rows
- **THEN** the strip disappears on the next frame, the visualizer stays on, and the strip comes back when the terminal is resized to 30 rows again

### Requirement: Spectrum bands
The bands SHALL be spaced logarithmically between about 40 Hz and 16 kHz. Each bar's height SHALL reflect the band's level on a decibel scale, from silence at about −60 dB to full height at 0 dB, with eighth-of-a-row steps. The level SHALL be measured on the decoded audio mixed to mono, before the volume is applied, so changing the volume SHALL NOT change the bars.

#### Scenario: Low tone
- **WHEN** a track containing only a 100 Hz tone is playing
- **THEN** the tallest bar is in the left third of the strip

#### Scenario: High tone
- **WHEN** a track containing only a 5 kHz tone is playing
- **THEN** the tallest bar is in the right half of the strip

#### Scenario: Volume does not matter
- **WHEN** a track is playing and the user lowers the volume from 100% to 20%
- **THEN** the bars keep the same heights for the same audio

### Requirement: Bar motion
A bar SHALL rise to a higher level as soon as it is measured, and SHALL fall back gradually, taking about one second to fall from full height to zero.

#### Scenario: Sudden silence
- **WHEN** a loud passage is followed by silence
- **THEN** the bars shrink over about one second instead of disappearing at once

### Requirement: Bars follow playback state
The bars SHALL fall to zero, with the same gradual motion, when playback is paused or stopped, when nothing is loaded, and while a track is not producing audio. Seeking or changing track SHALL drop the audio measured before it, so the bars never show audio from an earlier position.

#### Scenario: Paused
- **WHEN** a track is playing and the user presses Space
- **THEN** the bars fall to zero and stay there until playback resumes

#### Scenario: Nothing played yet
- **WHEN** kasset has just started
- **THEN** the spectrum strip shows no bars

#### Scenario: Track change
- **WHEN** a loud track is playing and the user presses `n` to a quiet track
- **THEN** the bars reflect only the new track's audio once it has started

### Requirement: Toggle the visualizer
The visualizer SHALL start on, or as it was when kasset last quit (see *Remembered settings*). In Normal mode, from either pane, `v` SHALL turn it off or on. While the terminal is too short for the spectrum strip, `v` SHALL show a status message: that the visualizer is off, or that it is on and needs a taller terminal. The strip SHALL appear once the terminal is tall enough. The setting SHALL be kept across runs.

#### Scenario: Hide and show
- **WHEN** the spectrum strip is shown and the user presses `v`
- **THEN** the strip disappears and the panes take its rows, and after another `v` the strip is shown again

#### Scenario: Turned on while too short
- **WHEN** the terminal is 20 rows tall, the visualizer is off, and the user presses `v`
- **THEN** the status line says the visualizer needs a taller terminal, and after the terminal is resized to 30 rows the spectrum strip is shown

#### Scenario: Turned off while too short
- **WHEN** the terminal is 20 rows tall, the visualizer is on, and the user presses `v`
- **THEN** the status line says the visualizer is off, and resizing the terminal to 30 rows shows no spectrum strip

#### Scenario: Not while typing
- **WHEN** a search is being typed and the user presses `v`
- **THEN** `v` is added to the query and the visualizer is unchanged

#### Scenario: Listed in the help
- **WHEN** the user runs `kasset --help` or presses `?`
- **THEN** `v` is listed with the action "show / hide the visualizer"

#### Scenario: Kept across runs
- **WHEN** the user hides the visualizer, quits and starts kasset again
- **THEN** the spectrum strip is not shown until the user presses `v`

### Requirement: Visualizer refresh rate
While the spectrum strip is shown and a track is playing, the system SHALL redraw the screen at least 25 times per second. When the strip is not shown, or no track is playing and every bar has fallen to zero, the system SHALL NOT redraw more often than it does without the visualizer.

#### Scenario: Animating
- **WHEN** the spectrum strip is shown and a track is playing
- **THEN** the bars update at least 25 times per second

#### Scenario: Idle
- **WHEN** playback is paused and every bar has fallen to zero
- **THEN** the screen is redrawn no more often than four times per second
