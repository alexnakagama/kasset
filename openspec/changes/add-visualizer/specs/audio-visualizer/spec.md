# Spec Delta

## Purpose

Shows a live frequency spectrum of the audio kasset is playing, as bars in a pane next to the track list, so the screen reflects the music itself.

## ADDED Requirements

### Requirement: Spectrum pane
When the terminal is wide enough for three panes and the visualizer is on, the system SHALL show a pane titled "Spectrum" to the right of the main pane. The pane SHALL draw one vertical bar per frequency band, from the lowest band on the left to the highest on the right, filling the pane's height at full level. The pane SHALL never take keyboard focus.

#### Scenario: Pane shown on a wide terminal
- **WHEN** the terminal is 120 columns wide and kasset starts
- **THEN** a "Spectrum" pane is shown to the right of the main pane

#### Scenario: Tab skips the pane
- **WHEN** the spectrum pane is shown, the main pane has focus, and the user presses `Tab` twice
- **THEN** focus moves to the sidebar and back to the main pane, never to the spectrum pane

### Requirement: Spectrum bands
The bands SHALL be spaced logarithmically between about 40 Hz and 16 kHz. Each bar's height SHALL reflect the band's level on a decibel scale, from silence at about −60 dB to full height at 0 dB, with eighth-of-a-row steps. The level SHALL be measured on the decoded audio mixed to mono, before the volume is applied, so changing the volume SHALL NOT change the bars.

#### Scenario: Low tone
- **WHEN** a track containing only a 100 Hz tone is playing
- **THEN** the tallest bar is in the left third of the pane

#### Scenario: High tone
- **WHEN** a track containing only a 5 kHz tone is playing
- **THEN** the tallest bar is in the right half of the pane

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
- **THEN** the spectrum pane shows no bars

#### Scenario: Track change
- **WHEN** a loud track is playing and the user presses `n` to a quiet track
- **THEN** the bars reflect only the new track's audio once it has started

### Requirement: Toggle the visualizer
The visualizer SHALL be on when kasset starts. In Normal mode, from either pane, `v` SHALL turn it off or on. While the terminal is too narrow for three panes, `v` SHALL show a status message: that the visualizer is off, or that it is on and needs a wider terminal. The pane SHALL appear once the terminal is wide enough. The setting SHALL last until kasset quits.

#### Scenario: Hide and show
- **WHEN** the spectrum pane is shown and the user presses `v`
- **THEN** the pane disappears and the main pane takes its width, and after another `v` the pane is shown again

#### Scenario: Turned on while too narrow
- **WHEN** the terminal is 90 columns wide, the visualizer is off, and the user presses `v`
- **THEN** the status line says the visualizer needs a wider terminal, and after the terminal is resized to 120 columns the spectrum pane is shown

#### Scenario: Turned off while too narrow
- **WHEN** the terminal is 90 columns wide, the visualizer is on, and the user presses `v`
- **THEN** the status line says the visualizer is off, and resizing the terminal to 120 columns shows no spectrum pane

#### Scenario: Not while typing
- **WHEN** a search is being typed and the user presses `v`
- **THEN** `v` is added to the query and the visualizer is unchanged

#### Scenario: Listed in the help
- **WHEN** the user runs `kasset --help` or presses `?`
- **THEN** `v` is listed with the action "show / hide the visualizer"

### Requirement: Visualizer refresh rate
While the spectrum pane is shown and a track is playing, the system SHALL redraw the screen at least 25 times per second. When the pane is not shown, or no track is playing and every bar has fallen to zero, the system SHALL NOT redraw more often than it does without the visualizer.

#### Scenario: Animating
- **WHEN** the spectrum pane is shown and a track is playing
- **THEN** the bars update at least 25 times per second

#### Scenario: Idle
- **WHEN** playback is paused and every bar has fallen to zero
- **THEN** the screen is redrawn no more often than four times per second
