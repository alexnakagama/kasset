# Spec Delta

## MODIFIED Requirements

### Requirement: Switch between library and playlists
The sidebar SHALL list a "Library" entry first, then the "Artists", "Albums" and "Queue" entries, then the saved playlists ordered case-insensitively, each with its track count. The playlists SHALL be listed when kasset starts and listed again whenever the sidebar gains focus. `Enter` on a sidebar entry SHALL show it in the main pane and focus the main pane. The library SHALL keep its selection and filter while a playlist is shown.

#### Scenario: Playlists listed on start
- **WHEN** `Playlists` contains `Gym.m3u8` and `Running.m3u8` and kasset starts
- **THEN** the sidebar lists "Library", "Artists", "Albums", "Queue", "Gym" and "Running" without any key being pressed

#### Scenario: Open playlists view
- **WHEN** no playlist has been opened, the main pane shows the library with focus, and the user presses `Tab`
- **THEN** the sidebar has focus and lists the saved playlists, and the main pane still shows the library

#### Scenario: Back to library
- **WHEN** the library is filtered by "band" with its third row selected, the user opens a playlist, then selects "Library" in the sidebar and presses `Enter`
- **THEN** the main pane shows the library still filtered by "band" with the third row selected, and has focus

#### Scenario: Return to open playlist
- **WHEN** "Running" is shown filtered by "band" with its second row selected, and the user presses `Tab` twice
- **THEN** "Running" is still shown, filtered by "band" with the second row selected, and the main pane has focus

#### Scenario: Listing refreshed
- **WHEN** another program creates `New.m3u8` while the main pane has focus, and the user presses `Tab`
- **THEN** the sidebar lists "New"

#### Scenario: Open playlist removed meanwhile
- **WHEN** a playlist is shown in the main pane, another program deletes its file, and the user moves focus to the main pane
- **THEN** the main pane shows the library and the status area says the playlist no longer exists

#### Scenario: Filter the sidebar
- **WHEN** the sidebar has focus and the user types `/run` and Enter
- **THEN** the sidebar lists "Library", "Artists", "Albums", "Queue" and only the playlists whose names contain "run", with the first of those playlists selected

#### Scenario: Sidebar kept current
- **WHEN** the user adds a track from the library to a new playlist "Road trip" through "New playlist…", and then removes a track from "Running" with `d`
- **THEN** the sidebar lists "Road trip" with 1 track and shows the reduced count for "Running", without the focus having moved to the sidebar

### Requirement: Playing indicator in playlist views
The system SHALL mark the playing entry in a shown playlist only when that playlist is the play order and the current track is not a queued one, and the library SHALL mark the playing track whichever play order is active. While a track is loaded, the sidebar SHALL mark the entry that is the play order: "Library", a playlist, "Artists" for an artist or "Albums" for an album; while a queued track is current, it SHALL mark "Queue" instead (see the play-queue capability).

#### Scenario: Playlist playing
- **WHEN** "Running" is playing B and the user opens "Running"
- **THEN** B has the playing marker, and "Running" has the playing marker in the sidebar

#### Scenario: Different source
- **WHEN** the library is the play order and the user opens a playlist that contains the playing song
- **THEN** no entry in that playlist has the playing marker, and "Library" has the playing marker in the sidebar

#### Scenario: Album source
- **WHEN** a track is playing with an album as the play order
- **THEN** "Albums" has the playing marker in the sidebar, and "Library" does not

#### Scenario: Nothing played yet
- **WHEN** kasset has just started
- **THEN** no sidebar entry has the playing marker

#### Scenario: Queued track
- **WHEN** "Running" is the play order and a queued track is playing
- **THEN** "Queue" has the playing marker in the sidebar, "Running" does not, and no entry of "Running" has the playing marker
