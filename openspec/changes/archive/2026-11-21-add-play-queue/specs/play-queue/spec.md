# Spec Delta

## Purpose

Lets the user line up tracks to play next without changing what they are listening to: an up-next queue that plays before the current play order continues.

## ADDED Requirements

### Requirement: Add tracks to the queue
In Normal mode with the main pane focused, `e` SHALL add tracks to the end of the queue and `E` SHALL add them to its front, keeping their order. In the library, a playlist or a browsed track list this SHALL be the selected track. On an artist, album or "All tracks" row it SHALL be all of that entry's tracks, in the order `a` adds them. A notice SHALL say what was queued. With nothing selected, nothing SHALL happen.

#### Scenario: Queue a track
- **WHEN** "Song" is selected in the library, the queue is empty, and the user presses `e`
- **THEN** the queue holds "Song" and the status line says "Song" was queued

#### Scenario: Add to the end
- **WHEN** the queue holds A, B and the user presses `e` on C
- **THEN** the queue holds A, B, C

#### Scenario: Play next
- **WHEN** the queue holds A, B and the user presses `E` on C
- **THEN** the queue holds C, A, B

#### Scenario: Queue an album next
- **WHEN** the queue holds X, and the user presses `E` on the album "Moon Safari" (tracks 1 to 10) in the album list
- **THEN** the queue holds tracks 1 to 10 of "Moon Safari" in album order, then X

#### Scenario: Duplicates allowed
- **WHEN** the queue holds A and the user presses `e` on A
- **THEN** the queue holds A, A

#### Scenario: Nothing selected
- **WHEN** a filter matches no track and the user presses `e`
- **THEN** the queue is unchanged and no notice is shown

#### Scenario: Not in the sidebar or the queue view
- **WHEN** the sidebar has focus, or the queue is shown in the main pane, and the user presses `e` or `E`
- **THEN** the queue is unchanged

### Requirement: Queued tracks play first
When the current track ends, or the user presses `n`, and the queue is not empty, the system SHALL play the first queued track and take it out of the queue, instead of the next track of the play order. The play order SHALL stay where it was. Repeat one is the exception at a track's end: it replays the current track and the queue waits for `n`. With nothing current, `n` and Space SHALL play the first queued track.

#### Scenario: Queued track plays next
- **WHEN** an album lists A, B, C, A is playing from it, the queue holds X, and A ends
- **THEN** X plays and the queue is empty

#### Scenario: Next skips to the queue
- **WHEN** the library is the play order, track 3 is playing, the queue holds X, Y, and the user presses `n`
- **THEN** X plays and the queue holds Y

#### Scenario: Nothing played yet
- **WHEN** kasset has just started with nothing loaded, the user queues X and presses `n`
- **THEN** X plays

#### Scenario: Space with nothing loaded
- **WHEN** kasset starts with nothing to resume and a restored queue holding X, and the user presses Space
- **THEN** X plays

#### Scenario: Repeat one keeps the queue waiting
- **WHEN** repeat is "one", A is playing from an album, the queue holds X, and A ends
- **THEN** A plays again, the queue still holds X, and after `n` X plays

#### Scenario: Stop and play again
- **WHEN** queued track X is playing and the user presses `s` and then Space
- **THEN** X plays again from the start

### Requirement: The play order continues after the queue
When the queue is empty and a queued track ends or the user presses `n`, the system SHALL continue the play order with the track after the one it was on before the queue took over, following shuffle and repeat as usual. At the end of the play order with repeat off, playback SHALL stop.

#### Scenario: Back to the album
- **WHEN** an album lists A, B, C, A was playing from it, then queued X, Y played, and Y ends
- **THEN** B plays from the album

#### Scenario: Queue at the end of the play order
- **WHEN** C, the album's last track, is playing with repeat off, the queue holds X, and C ends
- **THEN** X plays, and when X ends playback stops

#### Scenario: Next on the last queued track at the end
- **WHEN** C, the album's last track, was playing with repeat off, queued X is now playing, the queue is empty, and the user presses `n`
- **THEN** X keeps playing, X is still the current track, and the status line says it is already the last track

#### Scenario: Queued track ends at the end
- **WHEN** C, the album's last track, was playing with repeat off, queued X then played, and X ends
- **THEN** playback stops with X as the current track, and Space plays X again

#### Scenario: Repeat all after the queue
- **WHEN** C, the album's last track, was playing with repeat all, queued X then played, and X ends
- **THEN** A plays from the album

### Requirement: Previous while a queued track plays
While a queued track is playing, `p` SHALL restart it when more than 3 seconds in, as for any track, and otherwise SHALL play the play order's track that was playing before the queue took over, from the start. The queued track SHALL NOT go back into the queue.

#### Scenario: Previous goes back to the play order
- **WHEN** A was playing from an album, queued X is playing at 0:02, and the user presses `p`
- **THEN** A plays from the start and the queue does not hold X

#### Scenario: Previous with no play order position
- **WHEN** kasset started with nothing loaded, the user queued X and pressed `n`, and X is at 0:02 when the user presses `p`
- **THEN** X restarts from 0:00

#### Scenario: Previous restarts a queued track
- **WHEN** queued X is playing at 1:10 and the user presses `p`
- **THEN** X restarts from 0:00

### Requirement: Shuffle and repeat with the queue
Queued tracks SHALL play in queue order whether shuffle is on or not; shuffle SHALL only apply to the play order. Repeat one SHALL replay the current track when it ends, a queued one too, so the queue waits until the user presses `n`. A track SHALL never go back into the queue by itself.

#### Scenario: Shuffle ignores the queue
- **WHEN** shuffle is on, the queue holds X, Y, Z, and the current track ends three times
- **THEN** X, Y and Z play in that order

#### Scenario: Repeat one on a queued track
- **WHEN** repeat is "one", queued X is playing, the queue holds Y, and X ends
- **THEN** X plays again, and after `n` Y plays

#### Scenario: Repeat all does not re-queue
- **WHEN** repeat is "all", the queue holds only X, X plays and ends
- **THEN** the queue is empty and the play order continues

### Requirement: Unplayable queued tracks
A queued track that cannot be opened or decoded SHALL be reported like any other and skipped: the next queued track plays, or the play order continues when the queue is empty. Skipped queued tracks SHALL NOT count toward stopping playback when every track of the play order fails.

#### Scenario: Corrupt queued track
- **WHEN** the queue holds a corrupt file X and then Y, and the current track ends
- **THEN** an error names X's file and Y plays

### Requirement: Playing from a list keeps the queue
Playing a track with `Enter` from the library, a playlist or a browse view SHALL make that list the play order as before, and SHALL leave the queue unchanged.

#### Scenario: Queue survives a new play order
- **WHEN** the queue holds X, and the user plays a track from the album "Moon Safari"
- **THEN** the queue still holds X, and X plays when that track ends

### Requirement: Queue view
The sidebar SHALL list a fixed "Queue" entry with the number of queued tracks. `Enter` on it SHALL show the queued tracks in the main pane, in queue order, with the library's columns, and focus the main pane. An empty queue SHALL say that it is empty. `/` SHALL filter the view as in the library. With no filter, `Esc` SHALL focus the sidebar with "Queue" selected.

#### Scenario: Open the queue
- **WHEN** the queue holds X, Y and the user presses `Enter` on "Queue" in the sidebar
- **THEN** the main pane lists X then Y and has focus, and the sidebar shows "Queue" with 2 tracks

#### Scenario: Empty queue
- **WHEN** the queue is empty and the user opens it
- **THEN** the main pane says the queue is empty

#### Scenario: View follows the queue
- **WHEN** the queue is shown with X, Y and X starts playing as the next track
- **THEN** the main pane lists only Y

#### Scenario: Esc from the queue
- **WHEN** the queue is shown and focused with no filter, and the user presses `Esc`
- **THEN** the sidebar has focus with "Queue" selected, and the queue is still shown

### Requirement: Edit the queue
In the queue view, `d` SHALL remove the selected entry, `J`/`K` SHALL move it down/up (only without a filter, as in a playlist), `Enter` SHALL take it out of the queue and play it now as a queued track, and `a`/`A` SHALL add the selected/listed tracks to a playlist as in the library. The play order SHALL not change. Keys SHALL act on the queue as it is when pressed, even right after the queue moved on.

#### Scenario: Remove from the queue
- **WHEN** the queue view lists X, Y, Z with Y selected and the user presses `d`
- **THEN** the queue holds X, Z

#### Scenario: Reorder the queue
- **WHEN** the queue view lists X, Y, Z with Z selected and the user presses `K`
- **THEN** the queue holds X, Z, Y and Z stays selected

#### Scenario: Reorder refused while filtered
- **WHEN** the queue view is filtered and the user presses `J`
- **THEN** the queue is unchanged and the status line says to clear the filter to reorder

#### Scenario: Selection follows the entry
- **WHEN** the queue view lists X, Y, Z with Z selected, and X starts playing as the next track
- **THEN** the view lists Y, Z with Z still selected, and `d` then removes Z

#### Scenario: Play a queued track now
- **WHEN** an album is the play order playing A, the queue view lists X, Y, Z with Y selected, and the user presses `Enter`
- **THEN** Y plays, the queue holds X, Z, and when Y ends X plays

### Requirement: Clear the queue
With the sidebar focused on "Queue", `D` SHALL ask for confirmation, and `y` SHALL empty the queue while any other key cancels. A queued track that is playing SHALL keep playing. With an empty queue, `D` SHALL do nothing.

#### Scenario: Clear
- **WHEN** the queue holds X, Y, "Queue" is selected in the sidebar, and the user presses `D` and then `y`
- **THEN** the queue is empty and the sidebar shows "Queue" with 0 tracks

#### Scenario: Cancel
- **WHEN** the user presses `D` on "Queue" and then `n`
- **THEN** the queue is unchanged

### Requirement: Queue size limit
The queue SHALL hold at most 1000 tracks. When adding would go over the limit, only the tracks that fit SHALL be added (the first ones in adding order), and the notice SHALL say how many were queued and that the queue is full.

#### Scenario: Queue full
- **WHEN** the queue holds 995 tracks and the user presses `e` on a 10-track album
- **THEN** the queue holds 1000 tracks ending with the album's first 5, and the status line says 5 of 10 tracks were queued and the queue is full

### Requirement: Playing markers with the queue
While a queued track is current, the sidebar SHALL mark "Queue" as playing, and no playlist entry, album, artist, "All tracks" or browsed track row SHALL have the playing marker. The library SHALL still mark the playing track. Once the play order continues, its markers SHALL return.

#### Scenario: Queued track playing
- **WHEN** an album is the play order and queued X is playing
- **THEN** "Queue" has the playing marker in the sidebar, "Albums" and the album's rows do not, and X has the playing marker in the library

#### Scenario: Back to the album marker
- **WHEN** the last queued track ends and the album continues with B
- **THEN** "Albums" has the playing marker in the sidebar again, and B has it in the album's track list

### Requirement: Queue key help
The bottom bar SHALL show `e/E queue` in the library, a playlist and a browsed track list, and `Enter play`, `d remove`, `J/K move` and `Esc sidebar` in the queue view. The help overlay and `kasset --help` SHALL list `e / E` and the queue view's keys.

#### Scenario: Queue view bar
- **WHEN** the queue is shown and focused
- **THEN** the bottom bar mentions `Enter` play, `d` remove and `J/K` move

#### Scenario: Help lists the queue keys
- **WHEN** the user runs `kasset --help`
- **THEN** the output lists `e / E` with adding to the end of the queue and playing next
