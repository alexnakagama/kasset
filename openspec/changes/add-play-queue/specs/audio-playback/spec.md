# Spec Delta

## MODIFIED Requirements

### Requirement: Next and previous track
The system SHALL move to the next or previous track in play order and start playing it; `n` SHALL play the first queued track instead while the queue holds tracks, and `p` on a queued track follows the play-queue capability. Pressing previous more than 3 seconds into a track SHALL restart the current track instead.

#### Scenario: Next track
- **WHEN** track 3 is playing and the user presses `n`
- **THEN** track 4 starts playing

#### Scenario: Previous restarts
- **WHEN** a track is at 1:10 and the user presses `p`
- **THEN** the same track restarts from 0:00

#### Scenario: Next takes the queue
- **WHEN** track 3 is playing, the queue holds X, and the user presses `n`
- **THEN** X starts playing

#### Scenario: Previous goes back
- **WHEN** a track is at 0:02 and the user presses `p`
- **THEN** the previous track in play order starts playing

### Requirement: Automatic advance
When the current track finishes, the system SHALL start the next track according to the repeat mode, or stop if there is no next track. Unless repeat is "one", a non-empty queue SHALL supply the next track first (see the play-queue capability).

#### Scenario: End of track
- **WHEN** track 3 ends with repeat off
- **THEN** track 4 starts automatically

#### Scenario: Queued track first
- **WHEN** track 3 ends with repeat off and the queue holds X
- **THEN** X starts automatically, and track 4 follows when X ends and the queue is empty

#### Scenario: End of library
- **WHEN** the last track ends with repeat off
- **THEN** playback stops
