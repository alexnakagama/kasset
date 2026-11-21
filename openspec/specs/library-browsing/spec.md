# library-browsing Specification

## Purpose

Lets the user browse the library by artist and by album from the sidebar, and
play one album or everything by one artist as its own play order.

## Requirements

### Requirement: Album grouping
Every track SHALL belong to exactly one album, identified by its album artist and its album title, both compared case-insensitively. A track's album artist SHALL be its album-artist tag when present, else its artist. Tracks whose album is "Unknown" SHALL form one "Unknown" album per album artist. An album or artist SHALL be shown with the spelling of its first track in library order.

#### Scenario: Compilation kept together
- **WHEN** three tracks have album "Hits" and album artist "Various Artists", with track artists "A", "B" and "C"
- **THEN** there is one album "Hits" by "Various Artists" with 3 tracks, and each track still shows its own artist

#### Scenario: Same title, different artists
- **WHEN** "Band" and "Other" each have an album titled "Greatest Hits"
- **THEN** there are two albums named "Greatest Hits", one under each artist

#### Scenario: Untagged files without folders
- **WHEN** five untagged files without an `Artist - Title` name sit directly in the library folder
- **THEN** they form one album "Unknown" by the artist "Unknown"

#### Scenario: Case differences merge
- **WHEN** one track has album "Moon Safari" by "Air" and another "moon safari" by "AIR"
- **THEN** both belong to one album

### Requirement: Artist and album entries in the sidebar
The sidebar SHALL list "Artists" and "Albums" right after "Library", before the playlists. `Enter` on "Artists" SHALL show the artist list in the main pane, and `Enter` on "Albums" SHALL show the album list. Either way the main pane SHALL get focus. Showing one of these closes any open playlist, as "Library" does. `Enter` on "Library" or on a playlist SHALL in turn replace a shown artist or album view in the main pane.

#### Scenario: Open the artist list
- **WHEN** the sidebar has focus with "Artists" selected and the user presses `Enter`
- **THEN** the main pane lists the artists and has focus

#### Scenario: Library after browsing
- **WHEN** the album "Moon Safari" is shown, the user moves focus to the sidebar, selects "Library" and presses `Enter`
- **THEN** the main pane shows the library and has focus

#### Scenario: Playlist after browsing
- **WHEN** the artist list is shown, the user moves focus to the sidebar, selects "Gym" and presses `Enter`
- **THEN** the main pane shows "Gym" and has focus

#### Scenario: Empty library
- **WHEN** the library has no tracks and the user presses `Enter` on "Artists" (or "Albums")
- **THEN** the main pane says there are no artists (or no albums), and navigation, `Enter` and `a` do nothing

#### Scenario: Back to the same place
- **WHEN** the user had opened the album "Moon Safari" through "Artists", moved focus to the sidebar, and presses `Enter` on "Artists" again
- **THEN** the main pane shows "Moon Safari" with the selection it had

### Requirement: Artist list
The artist list SHALL list every album artist once, sorted case-insensitively, each with its number of albums and tracks. An artist who only appears as the track artist on another artist's albums SHALL NOT be listed.

#### Scenario: Artists listed
- **WHEN** the library has 2 albums by "Air" (12 tracks) and 1 by "Björk" (10 tracks)
- **THEN** the artist list shows "Air" with 2 albums and 12 tracks, then "Björk" with 1 album and 10 tracks

#### Scenario: Guest on a compilation
- **WHEN** "C" appears only as the track artist of one track on "Various Artists"' compilation
- **THEN** "C" is not in the artist list

### Requirement: An artist's albums
`Enter` on an artist SHALL list an "All tracks" row first, then that artist's albums sorted case-insensitively by title, each with its track count.

#### Scenario: Open an artist
- **WHEN** the user presses `Enter` on "Air", who has albums "Talkie Walkie" and "Moon Safari"
- **THEN** the main pane lists "All tracks", "Moon Safari" and "Talkie Walkie", in that order

### Requirement: Album list
The album list SHALL list every album, sorted case-insensitively by album artist then title, each with its title, album artist and track count.

#### Scenario: Albums listed
- **WHEN** the library has "Moon Safari" by "Air" and "Debut" by "Björk"
- **THEN** "Moon Safari" (artist "Air") is listed before "Debut" (artist "Björk")

### Requirement: Album tracks
`Enter` on an album, in the album list or in an artist's albums, SHALL list its tracks with the same columns as the library, ordered by disc number, then track number, then title. Tracks without a number SHALL come after the numbered ones of their disc. `Enter` on "All tracks" SHALL list all of the artist's tracks, album by album in the order of the artist's albums.

#### Scenario: Two discs
- **WHEN** an album has tracks 1 and 2 on disc 1 and tracks 1 and 2 on disc 2
- **THEN** they are listed disc 1 track 1, disc 1 track 2, disc 2 track 1, disc 2 track 2

#### Scenario: All tracks
- **WHEN** the user presses `Enter` on "All tracks" for "Air" (albums "Moon Safari", then "Talkie Walkie")
- **THEN** every "Moon Safari" track is listed in album order, followed by every "Talkie Walkie" track

### Requirement: Going back
With the main pane focused on an artist's albums, an album, or "All tracks", `Backspace` SHALL go back one level, filter or not, and select the entry that was opened; the level left is forgotten. `Esc` SHALL clear an active filter first, else go back one level the same way. On the artist or album list, `Esc` (with no filter) SHALL move focus to the sidebar with that entry selected, and `Backspace` SHALL do nothing.

#### Scenario: Back from an album
- **WHEN** the user opened "Air", then "Moon Safari", and presses `Backspace`
- **THEN** the main pane lists Air's albums with "Moon Safari" selected

#### Scenario: Back to the sidebar
- **WHEN** the artist list is shown and focused with no filter, and the user presses `Esc`
- **THEN** the sidebar has focus with "Artists" selected, and the artist list is still shown

#### Scenario: Esc clears the filter first
- **WHEN** an album's tracks are filtered by "love" and the user presses `Esc`
- **THEN** the filter is cleared and the album is still shown

#### Scenario: Backspace goes back through a filter
- **WHEN** an album's tracks are filtered by "love" and the user presses `Backspace`
- **THEN** the album's parent list is shown with the album selected, and opening the album again lists all its tracks unfiltered

### Requirement: Filter and selection per level
`/` SHALL filter the shown level, case-insensitively: artists by name, albums by title or album artist, and tracks by title, artist or album as in the library. The "All tracks" row SHALL always stay listed, and a query SHALL select the first matching album rather than it. A query matching nothing SHALL show the same no-match message as the library. Each level SHALL keep its own filter and selection while the user goes into a deeper level and back.

#### Scenario: Filter artists
- **WHEN** the artist list is shown and the user types `/bj` and Enter
- **THEN** only artists whose names contain "bj" are listed

#### Scenario: Filter an artist's albums
- **WHEN** Air's albums are shown and the user types `/talk`
- **THEN** "All tracks" and "Talkie Walkie" are listed, with "Talkie Walkie" selected

#### Scenario: Filter kept while going deeper
- **WHEN** the artist list is filtered by "a", the user opens "Air" and then goes back
- **THEN** the artist list is still filtered by "a" with "Air" selected

### Requirement: Play an album or artist
`Enter` on a track in an album SHALL play it and make that album, in album order, the play order. `Enter` on a track in "All tracks" SHALL make all of that artist's tracks, in that order, the play order. Next, previous, automatic advance, shuffle and repeat SHALL then work within those tracks as they do for a playlist. Playing from the library or a playlist SHALL replace it as the play order.

#### Scenario: Album plays to its end
- **WHEN** an album lists A, B, C, B is playing from it with repeat off, and B ends
- **THEN** C plays, and when C ends playback stops

#### Scenario: Repeat all in an album
- **WHEN** an album lists A, B, C, it is the play order with repeat "all", and C ends
- **THEN** A plays

#### Scenario: Shuffle stays in the album
- **WHEN** shuffle is on, repeat is off, and the user plays a track of a 4-track album
- **THEN** the next three tracks played are the album's other three, each once, and then playback stops

#### Scenario: Artist play order spans albums
- **WHEN** the last track of Air's first album is playing from "All tracks" and it ends
- **THEN** the first track of Air's second album plays

### Requirement: Playing markers in browse views
While a track is loaded, the sidebar SHALL mark "Artists" when an artist is the play order and "Albums" when an album is. An album row SHALL have the playing marker when that album is the play order; an artist row when that artist is the play order or one of its albums is; the "All tracks" row when that artist is the play order. In a list of tracks, the playing track SHALL have the playing marker only when that album or artist is the play order.

#### Scenario: Album playing
- **WHEN** "Moon Safari" is the play order and the user opens the album list
- **THEN** "Moon Safari" has the playing marker, and so does "Albums" in the sidebar

#### Scenario: Album played through Artists
- **WHEN** the user plays "Moon Safari" reached through Artists → "Air", then goes back to the artist list
- **THEN** "Air" has the playing marker, "Moon Safari" has it among Air's albums, and the sidebar marks "Albums"

#### Scenario: Different play order
- **WHEN** the library is the play order and the user opens the album holding the playing track
- **THEN** no track in that album has the playing marker

### Requirement: Add browsed tracks to a playlist
In a list of tracks, `a` SHALL offer to add the selected track to a playlist and `A` every listed track, as in the library. On an artist, album or "All tracks" row, `a` SHALL offer to add all of that entry's tracks, in the order listed when it is opened.

#### Scenario: Add a whole album
- **WHEN** "Moon Safari" (10 tracks) is selected in the album list and the user presses `a` and picks "Chill"
- **THEN** the 10 tracks are added to "Chill" in album order, except those already in it

### Requirement: Browse key help
The bottom bar SHALL show `Enter` open, `a` add and `Esc` sidebar on the artist or album list; `Enter` open, `a` add and `Esc` back on an artist's albums; and `Enter` play, `a/A` add and `Esc` back on a list of tracks reached by browsing. The help overlay and `kasset --help` SHALL list `Backspace` and the browse use of `Esc`.

#### Scenario: Album tracks bar
- **WHEN** an album's tracks are shown and focused
- **THEN** the bottom bar mentions `Enter` play, `a/A` add and `Esc` back

#### Scenario: Artist list bar
- **WHEN** the artist list is shown and focused
- **THEN** the bottom bar mentions `Enter` open, `a` add and `Esc` sidebar

#### Scenario: Help lists back
- **WHEN** the user runs `kasset --help`
- **THEN** the output lists `Esc / Backspace` going back one level when browsing
