## MODIFIED Requirements

### Requirement: Playlist storage location
The system SHALL store each playlist as a UTF-8 `<name>.m3u8` file in a `Playlists` folder directly inside the library root. The folder SHALL be created only when the first playlist is saved. Only `.m3u8` files in that folder SHALL be treated as playlists.

#### Scenario: First playlist creates the folder
- **WHEN** the library root is `~/Music`, it has no `Playlists` folder, and the user creates a playlist named "Running"
- **THEN** the file `~/Music/Playlists/Running.m3u8` exists

#### Scenario: Other files ignored
- **WHEN** `Playlists` contains `Old.m3u`, `notes.txt` and `Gym.m3u8`
- **THEN** only "Gym" is listed as a playlist

#### Scenario: No folder
- **WHEN** the library root has no `Playlists` folder
- **THEN** the sidebar lists only "Library" and says there are no playlists yet, and nothing is written to disk

### Requirement: Switch between library and playlists
The sidebar SHALL list a "Library" entry first, then the saved playlists ordered case-insensitively, each with its track count. The playlists SHALL be listed when kasset starts and listed again whenever the sidebar gains focus. `Enter` on a sidebar entry SHALL show it in the main pane and focus the main pane. The library SHALL keep its selection and filter while a playlist is shown.

#### Scenario: Playlists listed on start
- **WHEN** `Playlists` contains `Gym.m3u8` and `Running.m3u8` and kasset starts
- **THEN** the sidebar lists "Library", "Gym" and "Running" without any key being pressed

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
- **THEN** the sidebar lists "Library" and only the playlists whose names contain "run", with the first of those playlists selected

#### Scenario: Sidebar kept current
- **WHEN** the user adds a track from the library to a new playlist "Road trip" through "New playlist…", and then removes a track from "Running" with `d`
- **THEN** the sidebar lists "Road trip" with 1 track and shows the reduced count for "Running", without the focus having moved to the sidebar

### Requirement: Open a playlist
With the sidebar focused, `Enter` on a playlist SHALL show its tracks in the main pane, in playlist order and with the same columns as the library. `Enter` on the playlist already shown SHALL keep its filter and selection. With a playlist shown and focused, `Esc` SHALL clear an active filter first, and otherwise move focus to the sidebar, leaving the playlist shown.

#### Scenario: Open
- **WHEN** "Running" with 3 tracks is selected in the sidebar and the user presses Enter
- **THEN** the main pane lists the 3 tracks in the order of the file and has focus

#### Scenario: Search inside a playlist
- **WHEN** a playlist is shown and focused and the user types `/band` and Enter
- **THEN** only that playlist's tracks matching "band" are listed

#### Scenario: Esc from a playlist
- **WHEN** a playlist is shown and focused with no filter, and the user presses Esc
- **THEN** the sidebar has focus with that playlist selected, the playlists are listed again, and the playlist is still shown in the main pane

#### Scenario: Reopen the shown playlist
- **WHEN** "Running" is shown filtered by "band", the user moves focus to the sidebar, selects "Running" and presses Enter
- **THEN** "Running" is still filtered by "band" with the same row selected

### Requirement: Rename a playlist
With the sidebar focused and a playlist selected in it, `R` SHALL ask for a new name, prefilled with the current one, and rename the playlist file. The same name rules as creation SHALL apply, except that changing only the letter case of the current name is allowed. `R` on the "Library" entry SHALL do nothing.

#### Scenario: Rename
- **WHEN** "Running" is selected in the sidebar, the user presses `R`, enters "Morning run" and presses Enter
- **THEN** the playlist is listed as "Morning run" and `Running.m3u8` no longer exists

#### Scenario: Cancel rename
- **WHEN** the rename prompt is open and the user presses Esc
- **THEN** the playlist keeps its name

#### Scenario: Rename the shown playlist
- **WHEN** "Running" is shown in the main pane and the user renames it to "Morning run" from the sidebar
- **THEN** the main pane still shows its tracks, now under the name "Morning run"

### Requirement: Delete a playlist
With the sidebar focused and a playlist selected in it, `D` SHALL ask for confirmation naming the playlist; `y` SHALL delete its file and any other key SHALL cancel. Deleting a playlist SHALL NOT delete any audio file. `D` on the "Library" entry SHALL do nothing.

#### Scenario: Confirm delete
- **WHEN** "Gym" is selected in the sidebar, the user presses `D` then `y`
- **THEN** "Gym" is no longer listed and `Gym.m3u8` is gone, while its songs remain in the library

#### Scenario: Cancel delete
- **WHEN** the user presses `D` then `n`
- **THEN** "Gym" is still listed

#### Scenario: Delete the shown playlist
- **WHEN** "Gym" is shown in the main pane and the user deletes it from the sidebar
- **THEN** the main pane shows the library

### Requirement: Playing indicator in playlist views
The system SHALL mark the playing entry in a shown playlist only when that playlist is the play order, and the library SHALL mark the playing track whichever play order is active. While a track is loaded, the sidebar SHALL mark the entry ("Library" or a playlist) that is the play order.

#### Scenario: Playlist playing
- **WHEN** "Running" is playing B and the user opens "Running"
- **THEN** B has the playing marker, and "Running" has the playing marker in the sidebar

#### Scenario: Different source
- **WHEN** the library is the play order and the user opens a playlist that contains the playing song
- **THEN** no entry in that playlist has the playing marker, and "Library" has the playing marker in the sidebar

#### Scenario: Nothing played yet
- **WHEN** kasset has just started
- **THEN** no sidebar entry has the playing marker

### Requirement: Key help
The bottom bar SHALL show the main keys valid for the focused pane, what it shows, and the current prompt or search; in Normal mode it SHALL end with `? help`. The help overlay and `kasset --help` SHALL list the playlist keys.

#### Scenario: Playlist view help
- **WHEN** a playlist is shown and the main pane has focus
- **THEN** the bottom bar mentions `d` remove and `J/K` move

#### Scenario: Sidebar help
- **WHEN** the sidebar has focus
- **THEN** the bottom bar mentions `Enter` open, `R` rename and `D` delete

#### Scenario: Help lists playlist keys
- **WHEN** the user runs `kasset --help`
- **THEN** the output lists `Tab`, `a / A`, `d`, `J / K`, `R` and `D` with their actions

#### Scenario: No help hint in prompts
- **WHEN** the rename prompt is open
- **THEN** the bottom bar shows the prompt's keys and no `? help`
