# Spec Delta

## MODIFIED Requirements

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
