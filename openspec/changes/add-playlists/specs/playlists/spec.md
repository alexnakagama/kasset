# Spec Delta

## Purpose

Lets users keep named playlists of songs from their library, saved inside the music folder so they persist between runs, and create, edit, rename, delete and play them from the terminal interface.

## ADDED Requirements

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
- **THEN** the playlists view opens and says there are no playlists yet, and nothing is written to disk

### Requirement: Portable playlist entries
The system SHALL write each entry as a path relative to the playlist file, using `/` as separator, and SHALL read relative or absolute entries. An entry SHALL first be matched literally; only if that fails SHALL `\` be read as a separator. Lines starting with `#` SHALL be treated as comments or extended-M3U tags.

#### Scenario: Relative paths written
- **WHEN** `~/Music/Band/LP/01 Song.flac` is added to "Running"
- **THEN** `Running.m3u8` contains the line `../Band/LP/01 Song.flac`

#### Scenario: Library moved
- **WHEN** the whole music folder is copied to another location and kasset is opened on the copy
- **THEN** every playlist lists the same tracks as before

#### Scenario: Absolute path read
- **WHEN** a hand-written playlist contains the absolute path of a library track
- **THEN** that track is listed in the playlist

#### Scenario: Backslash in a file name
- **WHEN** on Linux the library has `AC\DC - Song.mp3` and it is added to a playlist
- **THEN** after reopening kasset the playlist lists that track

#### Scenario: Windows-style separators
- **WHEN** a playlist written on Windows contains `..\Band\LP\01.flac` and that file exists as `Band/LP/01.flac`
- **THEN** the track is listed

### Requirement: File formatting preserved
When saving, the system SHALL keep a leading UTF-8 byte order mark, each existing line's ending (`\n` or `\r\n`) and the presence or absence of a final newline. New lines SHALL use the line ending of the file's first line (`\n` for new files).

#### Scenario: Windows file edited
- **WHEN** a playlist with a BOM and `\r\n` line endings has a track added
- **THEN** the file still starts with the BOM and every line, including the new one, ends with `\r\n`

#### Scenario: Unchanged round trip
- **WHEN** a playlist is opened and saved without changes to its entries
- **THEN** the file's bytes are identical to before

### Requirement: Playlists that cannot be edited safely
A playlist file that is not valid UTF-8, or that exceeds the size or entry limits (16 MiB, 100 000 entries), SHALL still be listed, opened (as far as read) and played, but its contents SHALL NOT be modified: removing (`d`), reordering (`J` / `K`) and adding to it SHALL be refused with a message saying why. Renaming (`R`) and deleting (`D`) it SHALL still be allowed, since neither rewrites the file's contents.

#### Scenario: Latin-1 file
- **WHEN** `Old.m3u8` contains bytes that are not valid UTF-8 and the user presses `d` inside it
- **THEN** nothing is removed, the file is unchanged and the status area says the playlist is not UTF-8 and cannot be edited

#### Scenario: Not offered as a target
- **WHEN** the user presses `a` and the only playlist cannot be edited
- **THEN** the picker shows it as read-only and choosing it is refused with the reason

#### Scenario: Rename and delete allowed
- **WHEN** `Old.m3u8` is not valid UTF-8 and the user renames it to "Older" with `R`, then deletes it with `D` and `y`
- **THEN** after the rename `Older.m3u8` has exactly the bytes `Old.m3u8` had, and after the delete it is gone

### Requirement: Unmatched entries preserved
Entries that do not match a track in the scanned library SHALL NOT be shown as tracks. They SHALL be counted in the status area when the playlist is opened, and SHALL be written back unchanged, in their original position, whenever the playlist is saved. Comment and tag lines SHALL also be kept.

#### Scenario: Missing file
- **WHEN** a playlist has 10 entries and one points to a file that no longer exists
- **THEN** 9 tracks are listed and the status area reports 1 entry not found in the library

#### Scenario: Edit keeps unmatched entries
- **WHEN** the user removes a track from a playlist that contains an unmatched entry
- **THEN** the saved file still contains the unmatched entry

### Requirement: Switch between library and playlists
`Tab` SHALL switch between the library side and the playlists side, each keeping its own state: `Tab` from the playlists list or an open playlist SHALL show the library with its previous selection and filter, and `Tab` from the library SHALL return to the playlists list, or to the open playlist with its own filter and selection. The playlists list SHALL show names, ordered case-insensitively, with track counts.

#### Scenario: Open playlists view
- **WHEN** no playlist has been opened, the library view is shown and the user presses `Tab`
- **THEN** the list of saved playlists is shown

#### Scenario: Back to library
- **WHEN** the playlists view or an open playlist is shown and the user presses `Tab`
- **THEN** the library view is shown with its previous selection and filter

#### Scenario: Return to open playlist
- **WHEN** "Running" is open filtered by "band" with its second row selected, and the user presses `Tab` twice
- **THEN** "Running" is shown again, still filtered by "band" with the second row selected

#### Scenario: Open playlist removed meanwhile
- **WHEN** the user returns with `Tab` to an open playlist whose file was deleted by another program
- **THEN** the playlists list is shown and the status area says the playlist no longer exists

### Requirement: Open a playlist
In the playlists view, `Enter` SHALL open the selected playlist, listing its tracks in playlist order with the same columns as the library. `Esc` SHALL return to the playlists view (after clearing an active filter first).

#### Scenario: Open
- **WHEN** "Running" with 3 tracks is selected and the user presses Enter
- **THEN** the 3 tracks are listed in the order of the file

#### Scenario: Search inside a playlist
- **WHEN** a playlist is open and the user types `/band` and Enter
- **THEN** only that playlist's tracks matching "band" are listed

### Requirement: Add tracks to a playlist
In the library view, `a` SHALL add the selected track and `A` SHALL add every track currently listed (after any search filter). Before adding, a picker SHALL let the user choose an existing playlist or "New playlist…"; `Esc` SHALL cancel. Tracks are appended at the end.

#### Scenario: Add one track
- **WHEN** "Song" is selected, the user presses `a` and picks "Running"
- **THEN** "Song" is the last track of "Running" and the status area says it was added

#### Scenario: Add filtered tracks
- **WHEN** the library is filtered to the 12 tracks of one album and the user presses `A` and picks "Running"
- **THEN** those 12 tracks are appended to "Running" in library order

#### Scenario: Cancel
- **WHEN** the picker is open and the user presses Esc
- **THEN** no playlist changes

#### Scenario: Entry limit
- **WHEN** a playlist has 99 990 entries and the user adds 20 new tracks to it
- **THEN** nothing is added and the status area says the playlist would exceed 100 000 entries

### Requirement: No duplicate additions
When adding, tracks already present in the target playlist SHALL be skipped and the status area SHALL say how many were skipped. Duplicates already present in a playlist file SHALL be kept and listed.

#### Scenario: Already present
- **WHEN** "Running" contains "Song" and the user adds "Song" to it again
- **THEN** "Running" still contains "Song" once and the status area says it was already there

### Requirement: Create a playlist
Choosing "New playlist…" in the picker SHALL ask for a name, then create the playlist with the tracks being added. Names SHALL be 1–100 characters and at most 200 bytes in UTF-8 after trimming surrounding spaces, SHALL NOT contain `/`, `\`, `:`, `*`, `?`, `"`, `<`, `>`, `|` or control characters, and SHALL NOT start with `.`. A name equal to an existing playlist's, ignoring case, SHALL be refused.

#### Scenario: New playlist
- **WHEN** the user picks "New playlist…", types "Road trip" and presses Enter
- **THEN** the playlist "Road trip" exists and contains the added tracks

#### Scenario: Invalid name
- **WHEN** the user types "a/b" as the name and presses Enter
- **THEN** the playlist is not created, the reason is shown, and the name can be edited

#### Scenario: Name taken
- **WHEN** "Running" exists and the user enters "running" as a new name
- **THEN** the playlist is not created and the status area says the name is already used

#### Scenario: Name too long in bytes
- **WHEN** the user enters a name of 70 Japanese characters (210 bytes in UTF-8)
- **THEN** the playlist is not created and the reason says the name is too long

#### Scenario: File appears meanwhile
- **WHEN** another program creates `Road trip.m3u8` after the name was accepted but before kasset writes it
- **THEN** that file is not overwritten and the status area says the name is already used

### Requirement: Remove a track from a playlist
With a playlist open, `d` SHALL remove the selected entry (only that occurrence) and save the playlist. The selection SHALL move to the entry that followed it, or the new last entry.

#### Scenario: Remove
- **WHEN** "Running" lists A, B, C, B is selected and the user presses `d`
- **THEN** "Running" lists A, C and C is selected

### Requirement: Reorder a playlist
With a playlist open and no search filter active, `J` SHALL move the selected entry one position down and `K` one position up, keeping it selected, and save the playlist. Moving past either end SHALL do nothing. With a filter active, these keys SHALL be refused with a message.

#### Scenario: Move down
- **WHEN** "Running" lists A, B, C, A is selected and the user presses `J`
- **THEN** "Running" lists B, A, C and A is still selected

#### Scenario: Filter active
- **WHEN** a filter is active inside a playlist and the user presses `K`
- **THEN** the order does not change and the status area says to clear the filter to reorder

### Requirement: Rename a playlist
In the playlists view, `R` SHALL ask for a new name, prefilled with the current one, and rename the playlist file. The same name rules as creation SHALL apply, except that changing only the letter case of the current name is allowed.

#### Scenario: Rename
- **WHEN** "Running" is selected, the user presses `R`, enters "Morning run" and presses Enter
- **THEN** the playlist is listed as "Morning run" and `Running.m3u8` no longer exists

#### Scenario: Cancel rename
- **WHEN** the rename prompt is open and the user presses Esc
- **THEN** the playlist keeps its name

### Requirement: Delete a playlist
In the playlists view, `D` SHALL ask for confirmation naming the playlist; `y` SHALL delete its file and any other key SHALL cancel. Deleting a playlist SHALL NOT delete any audio file.

#### Scenario: Confirm delete
- **WHEN** "Gym" is selected, the user presses `D` then `y`
- **THEN** "Gym" is no longer listed and `Gym.m3u8` is gone, while its songs remain in the library

#### Scenario: Cancel delete
- **WHEN** the user presses `D` then `n`
- **THEN** "Gym" is still listed

### Requirement: Immediate and safe saving
Every change to a playlist SHALL be saved to disk before the next key is handled. A save SHALL replace the file atomically, so the file on disk is always either the old or the new complete playlist.

#### Scenario: Quit right after edit
- **WHEN** the user adds a track and immediately presses `q`
- **THEN** the next run shows the playlist with that track

#### Scenario: Consecutive edits
- **WHEN** the user removes two tracks one after the other with nothing else touching the file
- **THEN** both removals are saved and no "changed on disk" message appears

### Requirement: Saving keeps file identity
Saving SHALL keep the playlist file's permission bits. When the playlist file is a symbolic link whose target lies inside the `Playlists` folder, the system SHALL save the file the link points to and keep the link.

#### Scenario: Symlinked playlist
- **WHEN** `Running.m3u8` is a symbolic link to `Playlists/archive/Running.m3u8` and the user adds a track
- **THEN** `Running.m3u8` is still a link and the target file contains the new track

#### Scenario: Permissions kept
- **WHEN** `Running.m3u8` has mode 0640 and the user removes a track
- **THEN** the saved file still has mode 0640

### Requirement: Links never lead writes outside the Playlists folder
A playlist that is a symbolic link whose resolved target (following every link in a chain) lies outside the resolved `Playlists` folder SHALL be read-only: it can be listed, opened, played, renamed and deleted (acting on the link only), but every addition and entry edit SHALL be refused with a message saying it links outside the `Playlists` folder. The system SHALL repeat the check immediately before every save.

#### Scenario: Link outside the Playlists folder
- **WHEN** `Mix.m3u8` is a symbolic link to `~/.bashrc` (for example in a downloaded music folder) and the user tries to add a track to "Mix" or remove an entry from it
- **THEN** "Mix" is marked read-only, the edit is refused with a message saying it links outside the `Playlists` folder, and `~/.bashrc` is unchanged

#### Scenario: Link repointed after opening
- **WHEN** "Mix" is open, linking inside `Playlists`, and another program repoints the link outside before the user adds a track
- **THEN** nothing is written to the new target, an error is shown, and "Mix" is reloaded and shown as read-only

### Requirement: Write failures
When a playlist cannot be saved, renamed or deleted (for example the music folder is read-only), the system SHALL show an error naming the playlist and leave the playlist as it was, both on disk and on screen. Playlists SHALL still be listable, openable and playable.

#### Scenario: Read-only folder
- **WHEN** the music folder is read-only and the user adds a track to "Running"
- **THEN** an error says "Running" could not be saved and "Running" is unchanged

### Requirement: External changes
If a playlist file was changed on disk by another program since kasset last read it, an edit SHALL NOT overwrite it: the system SHALL reload the playlist, discard the edit and say so in the status area.

#### Scenario: Edited elsewhere
- **WHEN** "Running" is open, another program appends a line to `Running.m3u8`, and the user presses `d`
- **THEN** no entry is removed, "Running" is shown with the other program's change, and the status area says it was reloaded

### Requirement: Play a playlist
`Enter` on a track of an open playlist SHALL play it and make that playlist the play order: next, previous, automatic advance, shuffle and repeat SHALL work within the playlist's tracks as they do for the library. `Enter` in the library view SHALL make the whole library the play order again.

#### Scenario: Advance within playlist
- **WHEN** a playlist lists A, B, C, B is playing with repeat off, and B ends
- **THEN** C plays, and when C ends playback stops

#### Scenario: Repeat all in playlist
- **WHEN** a playlist lists A, B, C, repeat is "all" and C ends
- **THEN** A plays

#### Scenario: Back to library order
- **WHEN** a playlist is playing and the user plays a track from the library view
- **THEN** next and previous follow the library order again

### Requirement: Editing the playing playlist
Every change to the playlist that is the play order (remove, reorder, tracks added from the library, reload after an external change) SHALL apply to the play order at once. Deleting or renaming it SHALL NOT interrupt playback. With shuffle on, added tracks SHALL be placed at random among the tracks not yet played.

#### Scenario: Reorder while playing
- **WHEN** a playlist lists A, B, C, A is playing and the user moves C above B
- **THEN** C plays after A

#### Scenario: Add to playing playlist
- **WHEN** "Running" lists A, B, B is playing with repeat off, and the user adds D to "Running" from the library
- **THEN** D plays when B ends

#### Scenario: Reload of playing playlist
- **WHEN** "Running" is playing B and another program changes `Running.m3u8` to A, B, X, after which kasset reloads it
- **THEN** B keeps playing and X plays when B ends

### Requirement: Removing the playing entry
Removing the entry that is playing SHALL let it finish without replaying it, even with repeat one. Next and the end of the track SHALL then go to the entry that followed it in play order (the shuffled order when shuffle is on), or, if it was the last, behave as at the end of the play order. Previous SHALL go to the entry that preceded it in play order, after the usual restart rule.

#### Scenario: Remove playing track
- **WHEN** a playlist lists A, B, C, B is playing and the user removes B
- **THEN** B keeps playing and C plays when B ends

#### Scenario: Repeat one
- **WHEN** a playlist lists A, B, C, B is playing with repeat one and the user removes B
- **THEN** C plays when B ends, and then C repeats

#### Scenario: Last entry removed
- **WHEN** a playlist lists A, B, C, C is playing and the user removes C
- **THEN** playback stops when C ends with repeat off, and A plays with repeat all

#### Scenario: Previous after removal
- **WHEN** a playlist lists A, B, C, B is playing at 0:02 and the user removes B and presses `p`
- **THEN** A plays

#### Scenario: Shuffled play order
- **WHEN** a playlist lists A, B, C, D, shuffle is on with play order C, B, D, A, B is playing and the user removes B
- **THEN** D plays when B ends; or, if the user instead presses `p` at 0:02, C plays

### Requirement: Playing indicator in playlist views
The system SHALL mark the playing entry in an open playlist only when that playlist is the play order, and the library view SHALL mark the playing track whichever play order is active.

#### Scenario: Playlist playing
- **WHEN** "Running" is playing B and the user opens "Running"
- **THEN** B has the playing marker

#### Scenario: Different source
- **WHEN** the library is the play order and the user opens a playlist that contains the playing song
- **THEN** no entry in that playlist has the playing marker

### Requirement: Key help
The keybinding bar SHALL show the keys valid in the current view or prompt, and `kasset --help` SHALL list the playlist keys.

#### Scenario: Playlist view help
- **WHEN** a playlist is open
- **THEN** the bottom bar mentions `d` remove, `J/K` move and `Esc` back
