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
The system SHALL show a track list, a now-playing panel (title, artist, album, state), a progress bar with elapsed and total time, the volume, shuffle and repeat indicators, and a bottom bar with the main keybindings.

#### Scenario: Track playing
- **WHEN** "Song" by "Band" is playing at 1:05 of 3:30
- **THEN** the now-playing panel shows "Song", "Band", the progress bar is about 31% full and reads "1:05 / 3:30"

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
