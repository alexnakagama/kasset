# Spec Delta

## Purpose

Shows the lyrics of the current track in a pane beside the track lists, following playback line by line when the lyrics are timed, so the user can read along without leaving kasset.

## ADDED Requirements

### Requirement: Lyrics pane
When the lyrics pane is on and the terminal is wide enough, the system SHALL show a pane titled "Lyrics" as a column to the right of the main pane, with the same height as the sidebar and the main pane. It SHALL show the lyrics of the current track (playing, paused, or last played). The pane SHALL never take keyboard focus and its border SHALL always use the unfocused style.

#### Scenario: Pane shown on a wide terminal
- **WHEN** the terminal is 120 columns wide and kasset starts
- **THEN** the sidebar, the main pane and a "Lyrics" pane are shown side by side, left to right

#### Scenario: Tab skips the pane
- **WHEN** the lyrics pane is shown, the main pane has focus, and the user presses `Tab` twice
- **THEN** focus moves to the sidebar and back to the main pane, never to the lyrics pane

#### Scenario: Spectrum below
- **WHEN** the terminal is 120 columns wide and 30 rows tall and the visualizer is on
- **THEN** the lyrics pane ends above the spectrum strip, and the strip spans the full width

### Requirement: Lyrics pane needs a wide terminal
The system SHALL show the lyrics pane only on terminals at least 100 columns wide. On a narrower terminal the screen SHALL look as it does without the lyrics pane. The pane SHALL take about a third of the width, never less than 28 columns and never more than 48 columns, borders included.

#### Scenario: Too narrow
- **WHEN** the terminal is 90 columns wide and the lyrics pane is on
- **THEN** only the sidebar and the main pane are shown

#### Scenario: Narrowing the terminal
- **WHEN** the lyrics pane is shown and the terminal is resized from 120 to 90 columns
- **THEN** the pane disappears on the next frame, stays on, and comes back when the terminal is resized to 120 columns again

#### Scenario: Width bounds
- **WHEN** the terminal is 100 columns wide, and later 200 columns wide
- **THEN** the lyrics pane is 33 columns wide, and later 48 columns wide

### Requirement: Lyrics sources
The system SHALL use the readable regular file next to the current track with its name and a `.lrc` extension in any case (an unreadable one counts as absent; a readable one without lyrics hides tag lyrics), else the lyrics in its tags (Vorbis comment, MP4, ID3v2 unsynchronised). Lyrics SHALL be read when a track becomes current, not during the scan, and not again when seeking in the same track.

#### Scenario: Sidecar file
- **WHEN** `song.flac` starts playing and `song.lrc` is next to it
- **THEN** the pane shows the lyrics from `song.lrc`

#### Scenario: Upper-case extension
- **WHEN** `song.mp3` starts playing and `song.LRC` is next to it
- **THEN** the pane shows the lyrics from `song.LRC`

#### Scenario: Sidecar wins over tags
- **WHEN** `song.flac` has lyrics in its tags and `song.lrc` is next to it
- **THEN** the pane shows the lyrics from `song.lrc`

#### Scenario: Tag lyrics
- **WHEN** `song.flac` starts playing, has timed lyrics in its tags, and no `.lrc` file is next to it
- **THEN** the pane shows the tag lyrics, following playback

#### Scenario: Empty sidecar
- **WHEN** `song.flac` has lyrics in its tags and an empty `song.lrc` is next to it
- **THEN** the pane shows "No lyrics"

#### Scenario: Unreadable sidecar
- **WHEN** `song.flac` has lyrics in its tags and `song.lrc` next to it cannot be read
- **THEN** the pane shows the tag lyrics

#### Scenario: Next track
- **WHEN** a track with lyrics is playing and the user presses `n` to a track without lyrics
- **THEN** the pane no longer shows the first track's lyrics

### Requirement: LRC format
The system SHALL read lyrics in the LRC format. A line SHALL be shown at every `[mm:ss]`, `[mm:ss.x]`, `[mm:ss.xx]` or `[mm:ss.xxx]` timestamp it starts with, ordered by time whatever the file order. An `[offset:±N]` header SHALL shift every time by N ms, a positive N showing lines earlier. Other `[tag:value]` headers SHALL be ignored; bracket text without a colon (like `[Chorus]`) SHALL be kept as text. Word timings (`<mm:ss.xx>`) SHALL be removed from the text.

#### Scenario: Several timestamps
- **WHEN** the file contains `[00:12.00][01:30.00]Chorus`
- **THEN** "Chorus" is the current line at 0:12 and again at 1:30

#### Scenario: Lines out of order
- **WHEN** the file lists the line timed 0:20 before the line timed 0:10
- **THEN** the line timed 0:10 is shown above the line timed 0:20

#### Scenario: Offset
- **WHEN** the file has `[offset:+500]` and a line timed `[00:10.00]`
- **THEN** that line becomes current at 0:09.5

#### Scenario: Header and word timings
- **WHEN** the file starts with `[ar:Band]` and contains `[00:05.00]<00:05.00>Hello <00:05.50>world`
- **THEN** "Band" is not shown as a lyric line and the line reads "Hello world"

#### Scenario: Section markers
- **WHEN** plain lyrics contain the lines `[Chorus]` and `[Verse 1] la la`
- **THEN** both lines are shown as written

#### Scenario: Blank timed line
- **WHEN** the file contains `[00:30.00]` with no text
- **THEN** an empty row is shown at that point, and no earlier line is highlighted from 0:30

#### Scenario: Blank last line
- **WHEN** the file ends with `[03:40.00]` with no text and the track is at 3:45
- **THEN** no lyric line is highlighted

### Requirement: Synced lyrics follow playback
For timed lyrics, the current line SHALL be the last line whose time is at or before the playback position. It SHALL be highlighted with the playing style and kept in the middle of the pane where possible. Earlier lines SHALL be dimmed. Before the first line's time, no line SHALL be highlighted. The highlight SHALL move within 0.1 s of a line's time while playing, and SHALL follow seeking.

#### Scenario: Current line highlighted
- **WHEN** a track with timed lyrics is playing at 0:15, between lines timed 0:12 and 0:18
- **THEN** the line timed 0:12 is highlighted near the middle of the pane, and the lines above it are dimmed

#### Scenario: Before the first line
- **WHEN** the first line is timed 0:20 and the track is at 0:05
- **THEN** no line is highlighted and the first lines are shown from the top of the pane

#### Scenario: Seeking
- **WHEN** a track is at 0:15 and the user presses Right to seek forward
- **THEN** the highlighted line is the one for the new position on the next frame

#### Scenario: Paused
- **WHEN** playback is paused
- **THEN** the highlighted line does not change until playback resumes

#### Scenario: Long line
- **WHEN** a lyric line is wider than the pane
- **THEN** it is wrapped onto several rows, all highlighted while it is current

### Requirement: Unsynced lyrics
Lyrics with no valid timestamps SHALL be shown as plain text in their file order, with no highlighted line. When they are taller than the pane, they SHALL scroll in proportion to the track's position, showing the start at the beginning of the track and the end at its end.

#### Scenario: Plain text lyrics
- **WHEN** the current track's lyrics have no timestamps
- **THEN** the lines are shown in file order with no line highlighted

#### Scenario: Scroll with progress
- **WHEN** the plain lyrics are three times taller than the pane and the track is half-way through
- **THEN** the pane shows the middle part of the lyrics

### Requirement: No lyrics
When nothing has been played yet, the pane SHALL show "Nothing playing". When the current track has no lyrics, or its lyrics could not be read, the pane SHALL stay in place and show "No lyrics". Both messages SHALL look like the message of an empty track list. "No lyrics" SHALL NOT be shown for a track whose lyrics have not been looked up yet.

#### Scenario: Track without lyrics
- **WHEN** a track with no `.lrc` file and no lyrics tag is playing
- **THEN** the pane shows "No lyrics" and the sidebar and main pane keep their widths

#### Scenario: Nothing played yet
- **WHEN** kasset has just started
- **THEN** the pane shows "Nothing playing"

#### Scenario: Widening with lyrics
- **WHEN** a track with lyrics is playing and the terminal is resized from 90 to 120 columns
- **THEN** no frame shows "No lyrics" before the lyrics appear

#### Scenario: Edited while hidden
- **WHEN** the user hides the pane with `l`, edits the current track's `.lrc`, and presses `l` again
- **THEN** the pane shows the edited lyrics

### Requirement: Lyrics are untrusted input
Lyrics SHALL be treated as untrusted. Control characters and terminal escape sequences SHALL never reach the screen. A lyrics file larger than 1 MiB SHALL be read only up to that size. A path that is not a regular file (a FIFO, a device, a directory) SHALL be ignored without blocking. Unreadable, malformed or non-UTF-8 lyrics SHALL never crash kasset or stop playback. The system SHALL never create, change or delete lyrics files or tags.

#### Scenario: Escape sequences
- **WHEN** a lyric line contains an ESC character followed by `[31m`
- **THEN** the line is shown without the escape sequence taking effect

#### Scenario: FIFO instead of a file
- **WHEN** `song.lrc` is a named pipe that nobody writes to
- **THEN** kasset keeps responding and falls back to the tag lyrics or "No lyrics"

#### Scenario: Not UTF-8
- **WHEN** `song.lrc` is encoded in Latin-1
- **THEN** the lyrics are shown with the undecodable characters replaced, and playback continues

#### Scenario: Absurd timestamps
- **WHEN** the file contains `[99999999999999999999:00.00]x` and `[offset:99999999999999999999]`
- **THEN** kasset does not crash, and the other lines of the file are still shown

#### Scenario: UTF-16 file
- **WHEN** `song.lrc` is UTF-16 with a byte-order mark
- **THEN** the lyrics are shown correctly

### Requirement: Toggle the lyrics pane
The lyrics pane SHALL be on when kasset starts. In Normal mode, from either pane, `l` SHALL turn it off or on. While the terminal is too narrow for the pane, `l` SHALL show a status message: that the lyrics pane is off, or that it is on and needs a wider terminal. The setting SHALL last until kasset quits.

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
