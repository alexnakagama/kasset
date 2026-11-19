## MODIFIED Requirements

### Requirement: Screen layout
The system SHALL show a sidebar (a "Library" entry followed by the saved playlists) and a main pane with the tracks of the entry chosen in it, above a now-playing area of at most three rows. On terminals tall enough for the spectrum strip, with the visualizer on, a spectrum strip SHALL be shown across the full width between the panes and the now-playing area. The now-playing area SHALL show the state, title, artist, album, the volume, shuffle and repeat indicators, and a progress bar with elapsed and total time. Below it SHALL be a status line and a bottom bar with the main keys for the current focus and mode; in Normal mode the bar SHALL end with `? help`.

#### Scenario: Track playing
- **WHEN** "Song" by "Band" is playing at 1:05 of 3:30
- **THEN** the now-playing area shows "Song" and "Band", "1:05" before the progress bar and "3:30" after it, and the bar is about 31% full

#### Scenario: Both panes visible
- **WHEN** the terminal is 100 columns wide and the main pane shows the library
- **THEN** the sidebar on the left lists "Library" and every saved playlist with its track count, and the main pane on the right lists the library's tracks

#### Scenario: Spectrum strip visible
- **WHEN** the terminal is 100 columns wide and 30 rows tall and the visualizer is on
- **THEN** the sidebar and the main pane are shown side by side, the spectrum strip spans the full width below them, and the now-playing area is below the strip

#### Scenario: Help hint always visible
- **WHEN** no prompt or search is open and the terminal is too narrow for the bottom bar's keys
- **THEN** the keys are cut and `? help` is still shown at the right end of the bar

#### Scenario: Indicators
- **WHEN** shuffle is on, repeat is "all" and the volume is 70%
- **THEN** the now-playing area shows that shuffle is on, "repeat all" and "70%"
