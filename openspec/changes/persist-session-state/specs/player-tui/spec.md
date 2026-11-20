# Spec Delta

## MODIFIED Requirements

### Requirement: kasset command
The system SHALL be invoked as `kasset [--no-state] [DIR]`, and SHALL support `--help` and `--version` without opening the TUI. `--no-state` SHALL run without reading or writing saved state (see *Run without saved state*).

#### Scenario: Help
- **WHEN** the user runs `kasset --help`
- **THEN** usage and keybinding summary are printed and the program exits with status 0

#### Scenario: Launch
- **WHEN** the user runs `kasset`
- **THEN** the full-screen player opens in the terminal

#### Scenario: Launch without saved state
- **WHEN** the user runs `kasset --no-state`
- **THEN** the full-screen player opens with the default settings and nothing resumed

### Requirement: Pane focus
Exactly one pane, the sidebar or the main pane, SHALL have keyboard focus. Navigation, search and `Enter` SHALL act on the focused pane only. `Tab` (or Shift+Tab) SHALL move focus to the other pane. The focused pane SHALL be marked by a highlighted border, and its selected row SHALL be highlighted more strongly than the selected row of the other pane.

#### Scenario: Tab moves focus
- **WHEN** the main pane has focus and the user presses `Tab`
- **THEN** the sidebar has focus, and the main pane keeps showing the same list with the same selection and filter

#### Scenario: Keys go to the focused pane
- **WHEN** the sidebar has focus and the user presses `j`
- **THEN** the sidebar selection moves down one row and the main pane's selection is unchanged

#### Scenario: Focus on start
- **WHEN** kasset starts with no playlist to restore
- **THEN** the main pane shows the library and has focus

#### Scenario: Focus on a restored playlist
- **WHEN** kasset starts and restores the playlist "Running" (see *Resume on start*)
- **THEN** the main pane shows "Running" and has focus
