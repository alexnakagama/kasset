# music-library Specification

## Purpose

Discovers the audio files a user has downloaded into a local folder and exposes them, with readable metadata, as the library the player works from.

## Requirements

### Requirement: Library folder resolution
The system SHALL use the folder given on the command line as the library root. When no folder is given, it SHALL use the user's music directory, and if that does not exist, the current working directory.

#### Scenario: Folder given as argument
- **WHEN** the user runs `kasset ~/Downloads/albums`
- **THEN** the library is loaded from `~/Downloads/albums`

#### Scenario: No folder given
- **WHEN** the user runs `kasset` with no arguments and `~/Music` exists
- **THEN** the library is loaded from `~/Music`

#### Scenario: Folder does not exist
- **WHEN** the user runs `kasset /no/such/dir`
- **THEN** the program exits with a non-zero status and an error message naming the missing folder, without entering the TUI

### Requirement: Recursive audio file discovery
The system SHALL scan the library root recursively and include every file whose extension is a supported audio format (mp3, flac, ogg, oga, wav, m4a, aac), case-insensitively. Other files SHALL be ignored.

#### Scenario: Nested folders
- **WHEN** the root contains `Artist/Album/01.flac` and `single.MP3`
- **THEN** both files appear in the library

#### Scenario: Non-audio files ignored
- **WHEN** the root contains `cover.jpg` and `notes.txt`
- **THEN** neither appears in the library

#### Scenario: Unreadable subfolder
- **WHEN** a subfolder cannot be read due to permissions
- **THEN** it is skipped and the rest of the library still loads

### Requirement: Track metadata
For each track the system SHALL read title, artist, album, track number and duration from embedded tags when present. When the title is missing it SHALL use the file name without extension; missing artist or album SHALL be shown as "Unknown".

#### Scenario: Tagged file
- **WHEN** a file has title "Song", artist "Band" and album "LP" tags
- **THEN** the library shows "Song", "Band" and "LP" for that track

#### Scenario: Untagged file
- **WHEN** `my track.mp3` has no tags
- **THEN** its title is "my track" and artist is "Unknown"

### Requirement: Library ordering
The system SHALL order tracks by artist, then album, then track number, then title, comparing text case-insensitively.

#### Scenario: Album order preserved
- **WHEN** an album has tracks numbered 1, 2 and 10
- **THEN** they are listed in order 1, 2, 10

### Requirement: Empty library
The system SHALL start normally when no supported files are found and show a message stating that no music was found in the folder.

#### Scenario: Empty folder
- **WHEN** the library root contains no audio files
- **THEN** the TUI opens and displays "No music found in <folder>"
