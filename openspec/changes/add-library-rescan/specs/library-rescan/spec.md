# Spec Delta

## Purpose

Lets the user bring the library up to date with the music folder while kasset runs, without restarting it or interrupting playback, and keeps every view, the play order and the queue consistent with the updated library.

## ADDED Requirements

### Requirement: Rescan on demand
The system SHALL rescan the library folder when the user presses `u` in Normal mode, whichever pane has focus. The rescan SHALL run in the background: the interface SHALL keep responding to keys and playback SHALL continue while it runs. While it runs, the status line SHALL say "Scanning the library…" unless another message is shown. The system SHALL NOT rescan on its own.

#### Scenario: Rescan while playing
- **WHEN** a track is playing and the user presses `u`
- **THEN** the status line says "Scanning the library…", the track keeps playing without a gap, and keys keep working until the rescan ends

#### Scenario: Key typed into a prompt
- **WHEN** the search prompt is open and the user types `u`
- **THEN** "u" is added to the query and no rescan starts

#### Scenario: No automatic rescan
- **WHEN** a file is copied into the library folder and the user does not press `u`
- **THEN** the library does not change

### Requirement: Rescan finds new, changed and removed files
A rescan SHALL discover files with the same rules as the startup scan. Files not in the library before SHALL be added; files no longer found SHALL be removed from it. A file whose size or modification time changed SHALL have its metadata read again; a file with neither changed SHALL keep the metadata read before, without its tags being read again.

#### Scenario: New album copied in
- **WHEN** the folder `Air/Moon Safari/` with 10 tracks is copied into the library and the user rescans
- **THEN** the 10 tracks appear in the library, in library order

#### Scenario: File deleted
- **WHEN** a track's file is deleted and the user rescans
- **THEN** the track is no longer listed in the library

#### Scenario: File retagged
- **WHEN** a track's title tag is changed from "Song" to "Song (Live)" and the user rescans
- **THEN** the library shows "Song (Live)" for it, in the position its new metadata sorts to

#### Scenario: Unchanged file not reread
- **WHEN** the user rescans and a file's size and modification time are the same as at the previous scan
- **THEN** its tags are not read again and its metadata is unchanged

### Requirement: Rescan summary
When a rescan has been applied, the system SHALL show a notice saying how many tracks were added, removed and changed (only the counts that are not zero), or that the library is unchanged. A track counts as changed only if what is shown or sorted for it changed. Files skipped or left out by the scan's limits SHALL be reported as at startup.

#### Scenario: Some changes
- **WHEN** a rescan adds 12 tracks, removes 3 and finds 5 retagged
- **THEN** the status line says "Library updated: 12 added, 3 removed, 5 changed"

#### Scenario: Nothing changed
- **WHEN** a rescan of a 340-track library finds no difference
- **THEN** the status line says "Library unchanged (340 tracks)"

#### Scenario: Touched but identical
- **WHEN** a file's modification time changed but its tags are the same
- **THEN** it is not counted as changed

### Requirement: One rescan at a time
Pressing `u` while a rescan is running SHALL NOT start another one; the system SHALL show a notice that the library is already being scanned. The running rescan SHALL finish as usual.

#### Scenario: Pressed twice
- **WHEN** the user presses `u` and presses it again before the rescan ends
- **THEN** a notice says the library is already being scanned, and only one summary is shown when it ends

### Requirement: A failed rescan keeps the library
When the rescan cannot read the library folder, or finds no supported files while the library has tracks, the system SHALL leave the library, every view, the play order and the queue unchanged and show an error notice. Quitting while a rescan runs SHALL NOT wait for it.

#### Scenario: Folder gone
- **WHEN** the library folder was deleted and the user rescans
- **THEN** an error naming the folder is shown and every track is still listed

#### Scenario: Drive unmounted
- **WHEN** the library is on a drive that was unmounted, leaving an empty folder, and the user rescans
- **THEN** an error says no music was found and the library was kept, and every track is still listed

#### Scenario: Quit during a rescan
- **WHEN** the user presses `q` while a rescan of a large library is running
- **THEN** kasset exits at once

### Requirement: Lists follow a rescan
After a rescan, the library list, the "Library" track count and the counts of the other sidebar entries SHALL reflect the updated library. Each list SHALL keep its filter, re-applied to its new contents, and keep its selection on the same track or entry when it is still listed; otherwise the selection SHALL stay at the same row, clamped to the list.

#### Scenario: Selection kept
- **WHEN** "Song" is selected in the library and a rescan adds tracks that sort before it
- **THEN** "Song" is still selected

#### Scenario: Selected track removed
- **WHEN** the selected track's file was deleted and the user rescans
- **THEN** the track at the same row is selected

#### Scenario: Filter kept
- **WHEN** the library is filtered by "air" and a rescan adds a track by "Air"
- **THEN** the filter is still "air" and the new track is listed

### Requirement: Browse views follow a rescan
After a rescan, the artist and album lists SHALL be rebuilt from the updated library. An open artist or album level SHALL stay open, showing its updated contents, when an artist or album of the same name (compared ignoring case) still exists; otherwise the view SHALL go back to the nearest level above it that still exists.

#### Scenario: Album gains a track
- **WHEN** the album "Moon Safari" is open and a rescan finds one more of its tracks
- **THEN** the album stays open and lists the new track in album order

#### Scenario: Album removed
- **WHEN** the album "Moon Safari" is open through "Albums" and all its files were deleted before a rescan
- **THEN** the main pane shows the album list, without "Moon Safari"

#### Scenario: New artist
- **WHEN** a rescan adds tracks whose album artist is "Daft Punk"
- **THEN** "Daft Punk" is listed under "Artists" in its sorted position

### Requirement: Playlists follow a rescan
After a rescan, every playlist's entries SHALL be matched again against the updated library: an entry whose file appeared SHALL match, and an entry whose track was removed SHALL become unmatched. A rescan SHALL NOT write any playlist file.

#### Scenario: Missing file restored
- **WHEN** a playlist has an unmatched entry for `Air/Moon Safari/01.flac`, that file is copied back, and the user rescans
- **THEN** the playlist lists that track and its unmatched count drops by one

#### Scenario: Playlist file untouched
- **WHEN** the user rescans after deleting a track that is in a playlist
- **THEN** the playlist no longer lists the track and its file on disk is unchanged

### Requirement: Playback continues through a rescan
A rescan SHALL NOT interrupt, restart or change the current track, even when its file was removed. The play order SHALL take in the rescan: a library play order follows the updated library order, a playlist's its re-matched entries, an artist's or album's its updated tracks, each keeping its current track and dropping removed tracks. When the playing artist or album no longer exists, the play order SHALL continue with its remaining tracks, no longer tied to it.

#### Scenario: Playing file deleted
- **WHEN** the playing track's file is deleted and the user rescans
- **THEN** the track keeps playing to its end, then the track after it in the play order plays

#### Scenario: New track joins the library play order
- **WHEN** the library is the play order and a rescan adds a track that sorts right after the playing one
- **THEN** that new track plays next

#### Scenario: Playing album removed
- **WHEN** an album is playing, all its files (including the playing one) are deleted, and the user rescans
- **THEN** the playing track continues, the "Albums" entry no longer has the playing marker, and playback stops after it

#### Scenario: Playing track's neighbours deleted
- **WHEN** the library is the play order and the playing track and the tracks right before it are deleted, and the user rescans
- **THEN** the playing track keeps playing to its end, then the first remaining track that followed it in the library plays

### Requirement: Queue follows a rescan
After a rescan, tracks that were removed SHALL be taken out of the up-next queue, keeping the order of the others. A queued track that is playing SHALL keep playing.

#### Scenario: Queued track removed
- **WHEN** three tracks are queued, the second one's file is deleted, and the user rescans
- **THEN** the queue lists the first and third tracks, in that order

### Requirement: Rescanned files are untrusted
A rescan SHALL treat file names, folder names and tags exactly as the startup scan does: their text SHALL be sanitised before display, a file whose tags cannot be read (including one that crashes the tag reader) SHALL fall back to inferred metadata without disturbing the interface, and the scan SHALL stay within the startup scan's limits on depth and number of files. A rescan SHALL NOT write anything.

#### Scenario: Hostile tag
- **WHEN** a file added before a rescan has a title tag with terminal control characters
- **THEN** they never reach the screen

#### Scenario: Corrupt file added
- **WHEN** a corrupt file whose tags make the tag reader fail is added before a rescan
- **THEN** it is listed with metadata inferred from its path and nothing is printed over the interface

### Requirement: Rescan key help
The help overlay and `kasset --help` SHALL list `u` as rescanning the library folder.

#### Scenario: Help lists the key
- **WHEN** the user opens the help overlay
- **THEN** it lists `u` with "rescan the library folder"
