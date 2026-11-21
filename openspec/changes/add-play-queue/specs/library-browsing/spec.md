# Spec Delta

## MODIFIED Requirements

### Requirement: Playing markers in browse views
While a track is loaded, the sidebar SHALL mark "Artists" when an artist is the play order and "Albums" when an album is. An album row SHALL have the playing marker when that album is the play order; an artist row when that artist is the play order or one of its albums is; the "All tracks" row when that artist is the play order. In a list of tracks, the playing track SHALL have the playing marker only when that album or artist is the play order. While a queued track is current, none of these rows SHALL have the playing marker, and neither "Artists" nor "Albums" in the sidebar (see the play-queue capability).

#### Scenario: Album playing
- **WHEN** "Moon Safari" is the play order and the user opens the album list
- **THEN** "Moon Safari" has the playing marker, and so does "Albums" in the sidebar

#### Scenario: Album played through Artists
- **WHEN** the user plays "Moon Safari" reached through Artists → "Air", then goes back to the artist list
- **THEN** "Air" has the playing marker, "Moon Safari" has it among Air's albums, and the sidebar marks "Albums"

#### Scenario: Different play order
- **WHEN** the library is the play order and the user opens the album holding the playing track
- **THEN** no track in that album has the playing marker

#### Scenario: Queued track playing
- **WHEN** "Moon Safari" is the play order, a queued track is playing, and the user opens Artists → "Air"
- **THEN** neither "Air" nor "Moon Safari" has the playing marker, and the sidebar marks "Queue", not "Albums"
