# Spec Delta

## MODIFIED Requirements

### Requirement: Toggle the lyrics pane
The lyrics pane SHALL start on, or as it was when kasset last quit (see *Remembered settings*). In Normal mode, from either pane, `l` SHALL turn it off or on. While the terminal is too narrow for the pane, `l` SHALL show a status message: that the lyrics pane is off, or that it is on and needs a wider terminal. The setting SHALL be kept across runs.

#### Scenario: Hide and show
- **WHEN** the lyrics pane is shown and the user presses `l`
- **THEN** the pane disappears and the main pane takes its columns, and after another `l` the pane is shown again

#### Scenario: Turned on while too narrow
- **WHEN** the terminal is 90 columns wide, the lyrics pane is off, and the user presses `l`
- **THEN** the status line says the lyrics pane needs a wider terminal, and after the terminal is resized to 120 columns the pane is shown

#### Scenario: Turned off while too narrow
- **WHEN** the terminal is 90 columns wide, the lyrics pane is on, and the user presses `l`
- **THEN** the status line says the lyrics pane is off

#### Scenario: Not while typing
- **WHEN** a search is being typed and the user presses `l`
- **THEN** `l` is added to the query and the lyrics pane is unchanged

#### Scenario: Listed in the help
- **WHEN** the user runs `kasset --help` or presses `?`
- **THEN** `l` is listed with the action "show / hide the lyrics"

#### Scenario: Kept across runs
- **WHEN** the user hides the lyrics pane, quits and starts kasset again in a 120-column terminal
- **THEN** the lyrics pane is not shown until the user presses `l`

### Requirement: No lyrics
When there is no current track (nothing played yet and nothing resumed), the pane SHALL show "Nothing playing". When the current track has no lyrics, or its lyrics could not be read, the pane SHALL stay in place and show "No lyrics". Both messages SHALL look like the message of an empty track list. "No lyrics" SHALL NOT be shown for a track whose lyrics have not been looked up yet.

#### Scenario: Track without lyrics
- **WHEN** a track with no `.lrc` file and no lyrics tag is playing
- **THEN** the pane shows "No lyrics" and the sidebar and main pane keep their widths

#### Scenario: Nothing played yet
- **WHEN** kasset has just started with nothing to resume
- **THEN** the pane shows "Nothing playing"

#### Scenario: Widening with lyrics
- **WHEN** a track with lyrics is playing and the terminal is resized from 90 to 120 columns
- **THEN** no frame shows "No lyrics" before the lyrics appear

#### Scenario: Edited while hidden
- **WHEN** the user hides the pane with `l`, edits the current track's `.lrc`, and presses `l` again
- **THEN** the pane shows the edited lyrics

#### Scenario: Resumed track
- **WHEN** kasset starts and resumes a track that has an `.lrc` file
- **THEN** the pane shows that track's lyrics while it is paused, not "Nothing playing"
