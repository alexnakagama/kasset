# player-tui Specification

## Purpose

Provides the `kasset` command and the full-screen, keyboard-driven terminal interface for browsing the local library and controlling playback.

## Requirements

### Requirement: kasset command
The system SHALL be invoked as `kasset [DIR]`, and SHALL support `--help` and `--version` without opening the TUI.

#### Scenario: Help
- **WHEN** the user runs `kasset --help`
- **THEN** usage and keybinding summary are printed and the program exits with status 0

#### Scenario: Launch
- **WHEN** the user runs `kasset`
- **THEN** the full-screen player opens in the terminal

### Requirement: Terminal restoration
The system SHALL restore the terminal to its original state (cursor, raw mode, alternate screen) on quit, on error, and on panic.

#### Scenario: Normal quit
- **WHEN** the user presses `q`
- **THEN** audio stops, the TUI closes and the shell prompt works normally

#### Scenario: Ctrl+C
- **WHEN** the user presses Ctrl+C
- **THEN** the program quits the same way as `q`

### Requirement: Screen layout
The system SHALL show a sidebar (a "Library" entry followed by the saved playlists) and a main pane with the tracks of the entry chosen in it, above a now-playing area of at most three rows. The now-playing area SHALL show the state, title, artist, album, the volume, shuffle and repeat indicators, and a progress bar with elapsed and total time. Below it SHALL be a status line and a bottom bar with the main keys for the current focus and mode; in Normal mode the bar SHALL end with `? help`.

#### Scenario: Track playing
- **WHEN** "Song" by "Band" is playing at 1:05 of 3:30
- **THEN** the now-playing area shows "Song" and "Band", "1:05" before the progress bar and "3:30" after it, and the bar is about 31% full

#### Scenario: Both panes visible
- **WHEN** the terminal is 100 columns wide and the main pane shows the library
- **THEN** the sidebar on the left lists "Library" and every saved playlist with its track count, and the main pane on the right lists the library's tracks

#### Scenario: Help hint always visible
- **WHEN** no prompt or search is open and the terminal is too narrow for the bottom bar's keys
- **THEN** the keys are cut and `? help` is still shown at the right end of the bar

#### Scenario: Indicators
- **WHEN** shuffle is on, repeat is "all" and the volume is 70%
- **THEN** the now-playing area shows that shuffle is on, "repeat all" and "70%"

### Requirement: Track list navigation
The system SHALL let the user move the selection with Up/Down and `k`/`j`, PageUp/PageDown, and Home/End (`g`/`G`), scrolling the list to keep the selection visible.

#### Scenario: Move down
- **WHEN** track 1 is selected and the user presses `j`
- **THEN** track 2 is selected

#### Scenario: Jump to end
- **WHEN** the user presses `G`
- **THEN** the last track is selected and visible

### Requirement: Current track indication
The system SHALL visually distinguish the currently playing track in the list from the selected track.

#### Scenario: Selection differs from playing
- **WHEN** track 2 is playing and the user moves the selection to track 5
- **THEN** track 2 keeps a playing marker and track 5 is highlighted as selected

### Requirement: Search filter
The system SHALL let the user press `/` to type a query that filters the list to tracks whose title, artist or album contains the query, case-insensitively. Enter SHALL keep the filter and Esc SHALL clear it.

#### Scenario: Filter
- **WHEN** the user types `/beat` and presses Enter
- **THEN** only tracks matching "beat" are listed

#### Scenario: Clear filter
- **WHEN** a filter is active and the user presses Esc
- **THEN** the full library is listed again

### Requirement: Live updates
The system SHALL refresh the progress display at least twice per second while playing and react to key presses without waiting for the refresh.

#### Scenario: Progress moves
- **WHEN** a track is playing and no key is pressed
- **THEN** the elapsed time advances on screen every second

### Requirement: Error messages
The system SHALL show transient error and status messages in the interface without exiting.

#### Scenario: Playback error shown
- **WHEN** a track fails to decode
- **THEN** a message naming the file is shown in the status area

### Requirement: Resize
The system SHALL redraw correctly when the terminal is resized.

#### Scenario: Window resized
- **WHEN** the user resizes the terminal
- **THEN** the layout adapts to the new size on the next frame

### Requirement: Pane focus
Exactly one pane, the sidebar or the main pane, SHALL have keyboard focus. Navigation, search and `Enter` SHALL act on the focused pane only. `Tab` (or Shift+Tab) SHALL move focus to the other pane. The focused pane SHALL be marked by a highlighted border, and its selected row SHALL be highlighted more strongly than the selected row of the other pane.

#### Scenario: Tab moves focus
- **WHEN** the main pane has focus and the user presses `Tab`
- **THEN** the sidebar has focus, and the main pane keeps showing the same list with the same selection and filter

#### Scenario: Keys go to the focused pane
- **WHEN** the sidebar has focus and the user presses `j`
- **THEN** the sidebar selection moves down one row and the main pane's selection is unchanged

#### Scenario: Focus on start
- **WHEN** kasset starts
- **THEN** the main pane shows the library and has focus

### Requirement: Narrow terminals
When the terminal is narrower than 70 columns, the system SHALL show only the focused pane, using the full width. Keys SHALL behave as in the two-pane layout.

#### Scenario: Narrow terminal
- **WHEN** the terminal is 60 columns wide and the main pane has focus
- **THEN** only the main pane is shown, and after `Tab` only the sidebar is shown

#### Scenario: Widening the terminal
- **WHEN** the terminal is resized from 60 to 100 columns
- **THEN** both panes are shown on the next frame and focus is unchanged

### Requirement: Help overlay
`?` SHALL open a popup listing every key with its action, grouped by topic, from Normal mode in either pane. While the popup is open, Up/Down, `k`/`j`, PageUp/PageDown and Home/End (`g`/`G`) SHALL scroll it, Ctrl+C SHALL quit, and any other key SHALL close it without doing anything else.

#### Scenario: Open and close help
- **WHEN** the user presses `?` and then `q`
- **THEN** the help popup is shown after `?`, and after `q` it closes and kasset keeps running

#### Scenario: Help does not fit
- **WHEN** the terminal is 15 rows tall and the user presses `?` then `j`
- **THEN** the popup shows the first keys that fit, and after `j` it scrolls down one line

#### Scenario: Not while typing
- **WHEN** a search is being typed and the user presses `?`
- **THEN** `?` is added to the query and no popup opens
