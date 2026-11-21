# Spec Delta

## MODIFIED Requirements

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
