# session-state Specification

## Purpose

Lets kasset pick up where the user left off: it remembers the player settings and, for each library folder, the track and position to resume, across runs.

## Requirements

### Requirement: State file location
The system SHALL keep its saved state in one file in the user's per-user state folder: `$XDG_STATE_HOME/kasset/state` (default `~/.local/state/kasset/state`) on Linux, `~/Library/Application Support/kasset/state` on macOS, and `%LOCALAPPDATA%\kasset\state` on Windows. It SHALL NOT write state into the library folder.

#### Scenario: State kept outside the music folder
- **WHEN** the user plays a track from `~/Music` and quits on Linux with `XDG_STATE_HOME` unset
- **THEN** `~/.local/state/kasset/state` exists and no new file appears in `~/Music`

#### Scenario: No state folder known
- **WHEN** the system cannot determine a per-user state folder
- **THEN** kasset starts with the default settings and nothing is saved, without an error at start

### Requirement: Remembered settings
The system SHALL remember the volume, the shuffle setting, the repeat mode, and whether the visualizer and the lyrics pane are on, and SHALL start with the remembered values whichever library folder is opened. A saved volume SHALL be rounded down to a 5% step. Each value that is missing or invalid SHALL start at its default: 100% volume, shuffle off, repeat off, visualizer and lyrics pane on.

#### Scenario: Settings restored
- **WHEN** the user sets the volume to 60%, turns shuffle on, sets repeat to "all", hides the visualizer, quits and starts kasset again
- **THEN** the volume is 60%, shuffle is on, repeat is "all" and the visualizer is hidden

#### Scenario: Settings shared across folders
- **WHEN** the user sets the volume to 40% in `~/Music`, quits, and runs `kasset /mnt/other`
- **THEN** the volume is 40%

#### Scenario: Volume between steps
- **WHEN** the state file sets the volume to 63
- **THEN** kasset starts at 60%

#### Scenario: First run
- **WHEN** kasset starts with no saved state
- **THEN** the volume is 100%, shuffle and repeat are off, and the visualizer and lyrics pane are on

### Requirement: Resume point per library folder
For each library folder (identified by its resolved absolute path), the system SHALL remember the current track, its position, whether it was played from the library, a named playlist, an artist or an album, and which playlist the main pane shows. It SHALL keep resume points for at most 20 folders, dropping the least recently opened one first.

#### Scenario: Separate folders keep separate resume points
- **WHEN** the user quits at "Song A" in `~/Music`, then quits at "Song B" in `/mnt/other`, then runs `kasset ~/Music`
- **THEN** "Song A" is the current track

#### Scenario: Oldest folder forgotten
- **WHEN** resume points are saved for 20 folders and the user quits in a 21st folder
- **THEN** the resume point of the folder opened least recently is gone and the other 20 are kept

#### Scenario: Nothing to resume
- **WHEN** the user starts kasset in a folder for the first time and quits without playing anything
- **THEN** the next start in that folder has nothing loaded

#### Scenario: Unresumable point is kept
- **WHEN** the saved track could not be resumed (its drive was not mounted) and the user quits without playing anything
- **THEN** the saved track and position are still in the state, and are resumed on a later start once the track is found again

#### Scenario: Playing replaces the resume point
- **WHEN** the saved track could not be resumed and the user plays another track and quits
- **THEN** the next start resumes the track the user played

#### Scenario: Album source remembered
- **WHEN** the user quits while playing "Sexy Boy" from the album "Moon Safari" by "Air"
- **THEN** the state holds "Sexy Boy", its position, and the album "Moon Safari" by "Air" as its source

### Requirement: Resume on start
When the opened folder has a resume point, the system SHALL load its track paused at the saved position, never playing on its own, with the saved play source. A track the scan no longer finds SHALL NOT be resumed. A saved source playlist, artist or album that no longer exists or no longer holds the track SHALL be replaced by the library as the source. Independently, the saved shown playlist SHALL be shown, focused, if it still exists; else the library is shown.

#### Scenario: Resumed paused
- **WHEN** the user quits while "Song" plays at 2:15 and starts kasset again in the same folder
- **THEN** "Song" is the current track, paused at 2:15, and pressing Space continues it from 2:15

#### Scenario: Resumed in its playlist
- **WHEN** the user quits while playing "Running" with "Running" shown, and starts kasset again
- **THEN** "Running" is shown in the main pane with focus, the current track is the same one, and next/previous follow "Running"

#### Scenario: Resumed in its album
- **WHEN** the user quits while playing from the album "Moon Safari" with the album shown, and starts kasset again
- **THEN** the library is shown in the main pane with focus, the current track is the same one, next/previous follow "Moon Safari", and "Albums" has the playing marker in the sidebar

#### Scenario: Album no longer holds the track
- **WHEN** the saved source is the album "Moon Safari" by "Air", but the track's album tag was changed to "Premiers Symptômes"
- **THEN** the track is resumed with the library as the play source

#### Scenario: Track gone
- **WHEN** the saved track was deleted from the folder before kasset starts, and "Running" was shown
- **THEN** nothing is loaded, "Running" is shown, and no error is reported

#### Scenario: Playlist gone
- **WHEN** the saved source and shown playlist are "Gym", whose file was deleted, and the track is still in the library
- **THEN** the track is resumed with the library as the play source and the library is shown

#### Scenario: Track no longer in the playlist
- **WHEN** the saved source is "Gym", which still exists but no longer lists the track
- **THEN** the track is resumed with the library as the play source, and "Gym" is still shown if it was the shown playlist

#### Scenario: Source and shown playlist differ
- **WHEN** the user quits while playing from "Gym" with the library shown
- **THEN** the next start shows the library, and next/previous follow "Gym"

#### Scenario: Position past the end
- **WHEN** the saved position is past the end of the track (the file was replaced by a shorter one)
- **THEN** the track is resumed paused at 0:00

#### Scenario: Position cannot be reached
- **WHEN** the track plays but seeking to the saved position fails
- **THEN** the track is resumed paused at 0:00

#### Scenario: Track cannot be opened
- **WHEN** the saved track exists but cannot be opened or decoded even from its start
- **THEN** nothing is loaded and the status line says the track could not be resumed

### Requirement: When state is saved
The system SHALL save the state when the user quits (`q`, Ctrl+C) and, within the 2-second grace period kasset allows after a termination signal, when it stops because of that signal; and, while running, at most every 30 seconds when the state changed since the last save. An emergency exit (a second termination signal, or a shutdown that outlasts the grace period) SHALL NOT save, so only the last periodic save survives it.

#### Scenario: Saved at quit
- **WHEN** the user changes the volume and presses `q` right away
- **THEN** the next start has the new volume

#### Scenario: Saved while running
- **WHEN** a track has been playing for 2 minutes and the process is then killed with SIGKILL
- **THEN** the next start resumes that track no more than 30 seconds before the point where it was killed

#### Scenario: Nothing changed
- **WHEN** kasset sits paused with no key pressed for 5 minutes
- **THEN** the state file is not rewritten during that time

### Requirement: Saving never gets in the way
A failed save SHALL NOT stop playback, block quitting or change the exit status. The terminal SHALL be restored before the final save is written. While running, the first failed save SHALL be reported once in the status line. A failed save at quit SHALL be reported on standard error after the terminal is restored.

#### Scenario: State folder not writable
- **WHEN** the state folder is a regular file and the user quits
- **THEN** kasset quits normally with status 0 and prints a message naming the state file and the reason

#### Scenario: Repeated failures are not repeated
- **WHEN** saving fails while running and keeps failing
- **THEN** the status line reports it once, not every 30 seconds

### Requirement: Saved state is untrusted
The system SHALL treat the state file as untrusted input. It SHALL read at most 64 KiB, only from a regular file, and SHALL never write a file larger than it reads. Unknown entries and invalid values SHALL be ignored one by one; a file that cannot be read or parsed SHALL mean starting with the defaults. The state SHALL only select tracks the scan found, playlists the listing found, and artists and albums of the scanned library, never open any other path, and any text from it shown on screen SHALL be sanitised.

#### Scenario: Damaged file
- **WHEN** the state file contains random bytes
- **THEN** kasset starts with the defaults and no error, and quitting replaces the file with a valid one

#### Scenario: Invalid value ignored
- **WHEN** the state file sets the volume to 250 and repeat to "all"
- **THEN** the volume starts at 100% and repeat is "all"

#### Scenario: Path outside the library
- **WHEN** the resume point names the track `../../etc/passwd`
- **THEN** nothing is loaded and that file is never opened

#### Scenario: Unknown album source
- **WHEN** the resume point's source names an album no scanned track belongs to
- **THEN** the track is resumed with the library as the play source and nothing else is read

#### Scenario: Long paths
- **WHEN** resume points are saved for 20 folders whose paths and tracks are long enough that all 20 do not fit in 64 KiB
- **THEN** the file holds the most recently used folders that fit, the current folder first, and reading it back loses nothing it holds

#### Scenario: Not a regular file
- **WHEN** the state file is a named pipe (FIFO)
- **THEN** kasset starts with the defaults without waiting on it

### Requirement: Newer state files are kept
When the state file was written by a newer format version that this version does not understand, the system SHALL start with the defaults, SHALL NOT write the file during the run, and SHALL say so once in the status line at start as information, not as a save error.

#### Scenario: Downgrade
- **WHEN** the state file declares a format version higher than this kasset supports, the user changes the volume and quits
- **THEN** the file is unchanged, nothing is printed on quit, and the exit status is 0

### Requirement: Safe state writes
The system SHALL replace the state file atomically, so a crash or power loss during a save leaves either the old or the new file. A symbolic link at the state file's path SHALL be replaced by the file, never written through. On Unix the file SHALL be readable and writable by its owner only. A save SHALL keep the resume points of other folders that another kasset process saved meanwhile.

#### Scenario: Owner-only permissions
- **WHEN** kasset saves the state on Linux
- **THEN** the state file's mode is `0600`

#### Scenario: Symlinked state file
- **WHEN** the state file is a symbolic link to another file and kasset saves
- **THEN** the state file is a regular file and the link's target is unchanged

#### Scenario: Two instances
- **WHEN** one kasset runs in `~/Music` and another in `/mnt/other`, and both quit
- **THEN** the state file holds resume points for both folders

### Requirement: Run without saved state
`kasset --no-state [DIR]` SHALL start with the default settings and nothing resumed, and SHALL neither read nor write the state file during that run.

#### Scenario: Private session
- **WHEN** the user runs `kasset --no-state`, changes the volume and quits
- **THEN** the state file is unchanged and the next plain `kasset` run has the volume saved before

#### Scenario: Listed in the help
- **WHEN** the user runs `kasset --help`
- **THEN** `--no-state` is listed

### Requirement: Queue remembered per library folder
For each library folder, the system SHALL remember the queued tracks in order and restore them on start, independently of the resume point. Queued tracks the scan no longer finds SHALL be dropped. Like a resume point, the queue SHALL be saved when it changed, at most every 30 seconds while running and at quit, and SHALL only select tracks the scan found.

#### Scenario: Queue restored
- **WHEN** the queue holds X, Y, Z when the user quits, and kasset starts again in the same folder
- **THEN** the queue holds X, Y, Z and the sidebar shows "Queue" with 3 tracks

#### Scenario: Queue per folder
- **WHEN** the user quits with X queued in `~/Music`, then quits with nothing queued in `/mnt/other`, then runs `kasset ~/Music`
- **THEN** the queue holds X

#### Scenario: Missing queued track dropped
- **WHEN** the queue held X, Y at quit and Y was deleted before the next start
- **THEN** the queue holds X and no error is shown

#### Scenario: Queue outside the library
- **WHEN** the state file queues the track `../../etc/passwd`
- **THEN** that entry is ignored and that file is never opened

#### Scenario: Older version reads the file
- **WHEN** a kasset version without the queue reads a state file that holds a queue
- **THEN** it ignores the queue lines, does not fail, and resumes the saved track

### Requirement: Saved queue fits the state file
The saved queue SHALL count toward the 64 KiB limit of the state file. When a folder's entry does not fit, queued tracks SHALL be dropped from the end of its queue first, before the resume point or the shown playlist is dropped or the folder is left out. A state file SHALL hold at most 1000 queued tracks per folder; further entries SHALL be ignored.

#### Scenario: Long queue
- **WHEN** 1000 tracks with long paths are queued and the user quits
- **THEN** the state file is at most 64 KiB, holds the resume point and the first queued tracks that fit, in order, and reading it back gives that queue

#### Scenario: Long queue in another folder
- **WHEN** `/mnt/other` was saved with a queue too long to fit beside `~/Music`'s entry, and the user then quits kasset in `~/Music`
- **THEN** the state file still holds `/mnt/other` with the first queued tracks that fit, in order

### Requirement: Resume a queued track
When a queued track is current at save time, the system SHALL remember it with its position, as well as the play order's track it was played after. On start it SHALL load the queued track paused at that position, and the play order SHALL continue after its remembered track once the queue is empty. If the queued track is not found, the play order's track SHALL be resumed at 0:00 instead.

#### Scenario: Queued track resumed
- **WHEN** queued X plays at 1:20 after A of the album "Moon Safari", the queue holds Y, and the user quits and starts again
- **THEN** X is the current track, paused at 1:20, "Queue" has the playing marker in the sidebar, and after X ends Y plays and then B of "Moon Safari"

#### Scenario: Queued track gone
- **WHEN** the saved current track was queued X after A of an album, and X was deleted before the next start
- **THEN** A is the current track, paused at 0:00, with the album as the play order

#### Scenario: Play order gone
- **WHEN** the saved current track was queued X after A, and A was deleted before the next start
- **THEN** X is the current track paused at its position, and the library is the play order
