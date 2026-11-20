# Spec Delta

## MODIFIED Requirements

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

### Requirement: Library ordering
The system SHALL order tracks by artist, then album, then disc number, then track number, then title, comparing text case-insensitively. Tracks without a disc number SHALL sort as disc 1.

#### Scenario: Album order preserved
- **WHEN** an album has tracks numbered 1, 2 and 10
- **THEN** they are listed in order 1, 2, 10

#### Scenario: Discs in order
- **WHEN** an album has track 1 of disc 2 and track 5 of disc 1
- **THEN** track 5 of disc 1 is listed first
