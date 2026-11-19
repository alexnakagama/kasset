## MODIFIED Requirements

### Requirement: Screen layout
The system SHALL show a sidebar (a "Library" entry followed by the saved playlists) and a main pane with the tracks of the entry chosen in it, above a now-playing area of at most three rows. On terminals wide enough for three panes, with the visualizer on, a spectrum pane SHALL be shown to the right of the main pane. The now-playing area SHALL show the state, title, artist, album, the volume, shuffle and repeat indicators, and a progress bar with elapsed and total time. Below it SHALL be a status line and a bottom bar with the main keys for the current focus and mode; in Normal mode the bar SHALL end with `? help`.

#### Scenario: Track playing
- **WHEN** "Song" by "Band" is playing at 1:05 of 3:30
- **THEN** the now-playing area shows "Song" and "Band", "1:05" before the progress bar and "3:30" after it, and the bar is about 31% full

#### Scenario: Both panes visible
- **WHEN** the terminal is 100 columns wide and the main pane shows the library
- **THEN** the sidebar on the left lists "Library" and every saved playlist with its track count, and the main pane on the right lists the library's tracks

#### Scenario: Three panes visible
- **WHEN** the terminal is 120 columns wide and the visualizer is on
- **THEN** the sidebar, the main pane and the spectrum pane are shown side by side, from left to right

#### Scenario: Help hint always visible
- **WHEN** no prompt or search is open and the terminal is too narrow for the bottom bar's keys
- **THEN** the keys are cut and `? help` is still shown at the right end of the bar

#### Scenario: Indicators
- **WHEN** shuffle is on, repeat is "all" and the volume is 70%
- **THEN** the now-playing area shows that shuffle is on, "repeat all" and "70%"

### Requirement: Narrow terminals
When the terminal is narrower than 110 columns, the system SHALL NOT show the spectrum pane. When it is narrower than 70 columns, the system SHALL show only the focused pane, using the full width. Keys SHALL behave the same at every width.

#### Scenario: Narrow terminal
- **WHEN** the terminal is 60 columns wide and the main pane has focus
- **THEN** only the main pane is shown, and after `Tab` only the sidebar is shown

#### Scenario: Widening the terminal
- **WHEN** the terminal is resized from 60 to 100 columns
- **THEN** both panes are shown on the next frame and focus is unchanged

#### Scenario: Too narrow for the spectrum
- **WHEN** the terminal is 100 columns wide and the visualizer is on
- **THEN** only the sidebar and the main pane are shown

#### Scenario: Narrowing the terminal
- **WHEN** the spectrum pane is shown and the terminal is resized from 120 to 90 columns
- **THEN** the spectrum pane disappears on the next frame, the visualizer stays on, and the pane comes back when the terminal is widened to 120 columns again
