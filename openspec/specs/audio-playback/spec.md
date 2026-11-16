# audio-playback Specification

## Purpose

Plays tracks from the local library through the system's default audio output and provides the transport controls a user needs to listen to music.

## Requirements

### Requirement: Play a track
The system SHALL play the selected track through the default audio output device when the user starts it, replacing any track currently playing.

#### Scenario: Start a track
- **WHEN** the user selects a track and presses Enter
- **THEN** that track starts playing from the beginning and becomes the current track

### Requirement: Pause and resume
The system SHALL toggle between paused and playing for the current track, keeping the playback position.

#### Scenario: Pause then resume
- **WHEN** a track is playing at 0:42 and the user presses Space twice
- **THEN** playback pauses at 0:42 and then resumes from 0:42

### Requirement: Stop
The system SHALL stop playback and reset the position of the current track to the start.

#### Scenario: Stop playback
- **WHEN** a track is playing and the user presses `s`
- **THEN** audio stops and the state shows "Stopped"

### Requirement: Next and previous track
The system SHALL move to the next or previous track in play order and start playing it. Pressing previous more than 3 seconds into a track SHALL restart the current track instead.

#### Scenario: Next track
- **WHEN** track 3 is playing and the user presses `n`
- **THEN** track 4 starts playing

#### Scenario: Previous restarts
- **WHEN** a track is at 1:10 and the user presses `p`
- **THEN** the same track restarts from 0:00

#### Scenario: Previous goes back
- **WHEN** a track is at 0:02 and the user presses `p`
- **THEN** the previous track in play order starts playing

### Requirement: Automatic advance
When the current track finishes, the system SHALL start the next track according to the repeat mode, or stop if there is no next track.

#### Scenario: End of track
- **WHEN** track 3 ends with repeat off
- **THEN** track 4 starts automatically

#### Scenario: End of library
- **WHEN** the last track ends with repeat off
- **THEN** playback stops

### Requirement: Seek
The system SHALL seek the current track forward or backward by 5 seconds, clamped to the track bounds.

#### Scenario: Seek forward
- **WHEN** a track is at 1:00 and the user presses Right
- **THEN** playback continues from 1:05

#### Scenario: Seek before start
- **WHEN** a track is at 0:03 and the user presses Left
- **THEN** playback continues from 0:00

### Requirement: Volume control
The system SHALL adjust volume in steps of 5% between 0% and 100%, starting at 100%.

#### Scenario: Lower volume
- **WHEN** volume is 100% and the user presses `-`
- **THEN** volume becomes 95% and audio is quieter

#### Scenario: Volume bounds
- **WHEN** volume is 100% and the user presses `+`
- **THEN** volume stays at 100%

### Requirement: Shuffle
The system SHALL support a shuffle mode that, when enabled, plays tracks in a random order where each track plays once before any repeats.

#### Scenario: Toggle shuffle
- **WHEN** the user presses `z`
- **THEN** shuffle toggles and next/previous follow the shuffled order

### Requirement: Repeat modes
The system SHALL cycle repeat mode through off, all and one. With "all" it SHALL wrap from the last track to the first; with "one" it SHALL replay the current track when it ends.

#### Scenario: Repeat all wraps
- **WHEN** repeat is "all" and the last track ends
- **THEN** the first track starts playing

#### Scenario: Repeat one
- **WHEN** repeat is "one" and the current track ends
- **THEN** the same track plays again from the start

### Requirement: Undecodable files
The system SHALL NOT crash when a track cannot be opened or decoded; it SHALL show an error for that track and skip to the next one.

#### Scenario: Corrupt file
- **WHEN** the user plays a corrupt mp3
- **THEN** an error message names the file and the next track starts, even when repeat is "one"

#### Scenario: Every track fails
- **WHEN** every track in the play order fails to decode in succession
- **THEN** playback stops instead of looping, and the last error stays visible

### Requirement: No audio device
The system SHALL exit with a clear error message and non-zero status if no audio output device is available at startup.

#### Scenario: No device
- **WHEN** `kasset` starts on a machine with no audio output
- **THEN** it prints "No audio output device available" and exits non-zero
