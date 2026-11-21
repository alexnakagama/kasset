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
For each track the system SHALL read title, artist, album, track number and duration from embedded tags when present. A field whose tag is missing or empty SHALL be inferred from the file name and folders as described in the inference requirements. A title that is still missing SHALL be the file name without extension; an artist or album that is still missing SHALL be shown as "Unknown". Tag values SHALL always win over inferred ones. The system SHALL also read the album artist and the disc number from tags when present; neither is inferred from paths.

#### Scenario: Tagged file
- **WHEN** a file has title "Song", artist "Band" and album "LP" tags
- **THEN** the library shows "Song", "Band" and "LP" for that track

#### Scenario: Untagged file
- **WHEN** `my track.mp3` has no tags and sits directly in the library folder
- **THEN** its title is "my track" and artist is "Unknown"

#### Scenario: Tags win over the file name
- **WHEN** `Other - Name.mp3` has artist tag "Band" and title tag "Song"
- **THEN** its artist is "Band" and its title is "Song"

#### Scenario: Album artist and disc read
- **WHEN** a file has album-artist tag "Various Artists" and disc number 2
- **THEN** the track's album artist is "Various Artists" and its disc is 2

#### Scenario: Album artist is untrusted text
- **WHEN** an album-artist tag contains terminal control characters
- **THEN** they never reach the screen, as for the other tag fields

### Requirement: Artist and title from the file name
The system SHALL split a track's file name (without extension) at the first space-surrounded hyphen, en dash or em dash. The text before it SHALL be the artist (used only when the artist tag is missing) and the text after it the title (used only when the title tag is missing). The split SHALL be skipped when either side would be empty.

#### Scenario: Artist - Title
- **WHEN** `Kendrick Lamar - dodger blue.mp3` has no tags
- **THEN** its artist is "Kendrick Lamar" and its title is "dodger blue"

#### Scenario: Only the first dash splits
- **WHEN** `A - B - C.mp3` has no tags
- **THEN** its artist is "A" and its title is "B - C"

#### Scenario: Unspaced hyphen is not a separator
- **WHEN** `Spider-Man Theme.mp3` has no tags and sits directly in the library folder
- **THEN** its artist is "Unknown" and its title is "Spider-Man Theme"

#### Scenario: Title tag present, artist tag missing
- **WHEN** `Band - whatever.mp3` has title tag "Song" and no artist tag
- **THEN** its artist is "Band" and its title is "Song"

#### Scenario: Artist tag present, title tag missing
- **WHEN** `Other - Name.mp3` has artist tag "Band" and no title tag
- **THEN** its artist is "Band" and its title is "Name"

### Requirement: Track number from the file name
When a file name begins with 1 to 3 digits followed by a spaced dash, a period and a space, or a single space, and something follows, the system SHALL use that number as the track number (only when the track-number tag is missing or zero; 0 is no number) and infer artist and title from the rest. A single space counts only when the digits start with `0`. A leading number SHALL never become the artist.

#### Scenario: Numbered file
- **WHEN** `03 - Song.flac` has no tags and sits directly in the library folder
- **THEN** its track number is 3, its title is "Song" and its artist is "Unknown"

#### Scenario: Numbered file with artist
- **WHEN** `01. Band - Song.mp3` has no tags
- **THEN** its track number is 1, its artist is "Band" and its title is "Song"

#### Scenario: Number that belongs to the artist
- **WHEN** `50 Cent - In Da Club.mp3` has no tags
- **THEN** it has no track number, its artist is "50 Cent" and its title is "In Da Club"

#### Scenario: Number that belongs to the title
- **WHEN** `99 Problems.mp3` has no tags and sits directly in the library folder
- **THEN** it has no track number and its title is "99 Problems"

#### Scenario: Zero is not a track number
- **WHEN** `00 - Intro.mp3` has no tags
- **THEN** it has no track number and its title is "Intro"

#### Scenario: Bare number name
- **WHEN** `01.mp3` has no tags
- **THEN** it has no track number and its title is "01"

### Requirement: Video suffixes removed from inferred titles
A title taken from the file name SHALL have video-release labels removed, case-insensitively: any bracketed group (round or square) whose words all come from the label vocabulary (official, music, lyric, lyrics, audio, video, visualizer, visualiser, hd, hq, 4k), and, within the unbracketed run of those words at the end, everything from its first "official" on. If removal would leave the title empty, the title SHALL be kept unchanged.

#### Scenario: Bracketed label
- **WHEN** `Kendrick Lamar - dodger blue (Official Audio).mp3` has no tags
- **THEN** its title is "dodger blue"

#### Scenario: Unbracketed label
- **WHEN** `Self Love by Metro Boomin Official Lyric Video.mp3` has no tags
- **THEN** its title is "Self Love by Metro Boomin"

#### Scenario: Other brackets are kept
- **WHEN** `Calling (Spider-Man Across the Spider-Verse).mp3` has no tags
- **THEN** its title is "Calling (Spider-Man Across the Spider-Verse)"

#### Scenario: Label in the middle
- **WHEN** `Miguel - All I Want Is You (Official Video) ft. J. Cole.mp3` has no tags
- **THEN** its title is "All I Want Is You ft. J. Cole"

#### Scenario: Unbracketed words without "official" are kept
- **WHEN** `Band - Born to the Music.mp3` has no tags
- **THEN** its title is "Born to the Music"

#### Scenario: Dangling dash before a trailing label
- **WHEN** `Band - Song - Official Video.mp3` has no tags
- **THEN** its artist is "Band" and its title is "Song"

#### Scenario: Label-only name
- **WHEN** `Official Video.mp3` has no tags
- **THEN** its title is "Official Video"

### Requirement: Album and artist from folders
When a track lies at least two folders below the library folder (`<library>/<A>/<B>/<file>`), the system SHALL use folder `B` as the album when the album tag is missing, and folder `A` as the artist when the artist tag is missing and the file name gave no artist. Tracks with fewer than two such folders SHALL get nothing from folders.

#### Scenario: Artist/Album layout
- **WHEN** `<library>/Band/LP/01 Song.flac` has no tags
- **THEN** its artist is "Band", its album is "LP", its track number is 1 and its title is "Song"

#### Scenario: Deeper layout uses the nearest two folders
- **WHEN** `<library>/Rock/Band/LP/Song.mp3` has no tags
- **THEN** its artist is "Band" and its album is "LP"

#### Scenario: File name artist wins over folder
- **WHEN** `<library>/Various/Mix/Guest - Song.mp3` has no tags
- **THEN** its artist is "Guest" and its album is "Mix"

#### Scenario: One folder deep
- **WHEN** `<library>/Downloads/Song.mp3` has no tags
- **THEN** its artist and album are "Unknown"

### Requirement: Disc folders skipped for folder inference
When a track's parent folder is named like a disc (`CD`, `Disc` or `Disk`, any case, optionally followed by a space, `-` or `_`, then 1 or 2 digits), the system SHALL ignore that folder when inferring album and artist from folders, and use the folders above it instead.

#### Scenario: Disc folder is skipped
- **WHEN** `<library>/Band/LP/CD1/01 Song.flac` has no tags
- **THEN** its artist is "Band", its album is "LP" and its track number is 1

#### Scenario: Disc folder alone is not enough
- **WHEN** `<library>/LP/Disc 2/Song.mp3` has no tags
- **THEN** its artist and album are "Unknown"

### Requirement: Inferred text is untrusted
Text inferred from file and folder names SHALL be sanitised exactly like tag text before it is stored, shown, sorted, searched or written to a playlist. A part that is empty after sanitising SHALL count as missing.

#### Scenario: Control characters in a file name
- **WHEN** an untagged file is named `Ba\x1b[31mnd - Song.mp3`
- **THEN** its artist is shown without the escape character

### Requirement: Library ordering
The system SHALL order tracks by artist, then album, then disc number, then track number, then title, comparing text case-insensitively. Tracks without a disc number SHALL sort as disc 1.

#### Scenario: Album order preserved
- **WHEN** an album has tracks numbered 1, 2 and 10
- **THEN** they are listed in order 1, 2, 10

#### Scenario: Discs in order
- **WHEN** an album has track 1 of disc 2 and track 5 of disc 1
- **THEN** track 5 of disc 1 is listed first

### Requirement: Empty library
The system SHALL start normally when no supported files are found and show a message stating that no music was found in the folder.

#### Scenario: Empty folder
- **WHEN** the library root contains no audio files
- **THEN** the TUI opens and displays "No music found in <folder>"
