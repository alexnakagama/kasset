# Spec Delta

## ADDED Requirements

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
